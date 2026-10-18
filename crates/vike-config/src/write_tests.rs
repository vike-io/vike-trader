use super::*;
use std::assert_matches;

fn dir() -> tempfile::TempDir {
    tempfile::tempdir().expect("temp settings dir")
}

/// `SettingsSection::parse` round-trips every section's own word and accepts nothing else.
#[test]
fn a_section_parses_from_its_own_word_and_nothing_else() {
    for s in SettingsSection::ALL {
        assert_eq!(SettingsSection::parse(s.section()), Some(s));
    }
    assert_eq!(SettingsSection::parse("settings"), None);
    assert_eq!(SettingsSection::parse("Policy"), None, "case-sensitive");
    assert!(unknown_section_message("settings").contains("policy"));
}

/// The dotted-key contract: the section prefix is mandatory, the bare section name alone is
/// refused, and a foreign section is refused.
#[test]
fn the_key_spelling_is_the_read_halves_dotted_form() {
    for (key, needle) in [
        ("tradehub_addr", "spelled `config.<key>`"),
        ("config", "names the whole config section"),
        ("policy.max_leverage", "spelled `config.<key>`"),
        ("config..x", "empty key segment"),
    ] {
        let err = key_path(SettingsSection::Config, key).expect_err("a malformed key must refuse");
        assert_matches!(err, RowPlanError::BadKey { .. }, "{key}: {err:?}");
        assert!(err.to_string().contains(needle), "{key}: {err}");
    }
}

/// **The retired arming keys are plain rows now, and the validator refuses them.** No key grammar
/// diverts `policy.venues.<venue>` or `policy.accounts.<venue>.<LABEL>` any more (decision 0119:
/// an account's tier is its `account` row), so each resolves to an ordinary `setting` row — and
/// the write is refused before a row lands, because `Policy` has no such key.
#[test]
fn a_retired_arming_key_is_a_plain_row_the_validator_refuses() {
    assert_matches!(
        row_change_for("policy.venues.binance", "live").unwrap(),
        vike_secrets::RowChange::Setting { section, key, .. }
            if section == "policy" && key == "venues.binance"
    );
    let d = seeded();
    let db = vike_secrets::db_path_in(d.path());
    let before = std::fs::read(&db).expect("the database");
    for key in ["policy.venues.binance", "policy.accounts.hyperliquid.ALT"] {
        let err = write_setting_row(d.path(), key, "live", std::time::Duration::from_millis(500))
            .expect_err("a retired arming key must not land");
        assert_matches!(err, RowPlanError::Refused(_), "{key}: {err:?}");
    }
    assert_eq!(std::fs::read(&db).expect("the database"), before, "byte-identical");
}

/// Value typing: TOML-parseable text lands typed (as a JSON scalar), everything else lands as
/// a JSON string.
#[test]
fn values_are_typed_when_parseable_and_strings_otherwise() {
    assert_matches!(
        row_change_for("flags.reconcile", "true").unwrap(),
        vike_secrets::RowChange::Setting { value, .. } if value == "true"
    );
    assert_matches!(
        row_change_for("config.tradehub_addr", "127.0.0.1:7879").unwrap(),
        vike_secrets::RowChange::Setting { value, .. } if value == "\"127.0.0.1:7879\""
    );
}

/// **THE END-TO-END PROOF: a write lands one row, and reading it back sees the new value.**
#[test]
fn a_write_lands_one_row_in_the_database() {
    let d = dir();
    vike_secrets::plant_settings_rows(
        d.path(),
        &StoredSettings {
            settings: vec![vike_secrets::SettingRow {
                section: "preferences".to_string(),
                key: "log_level".to_string(),
                value: "\"info\"".to_string(),
            }],
            ..Default::default()
        },
    )
    .expect("a fresh store plants");

    let report = write_setting_row(
        d.path(),
        "policy.max_notional_per_order",
        "250",
        std::time::Duration::from_millis(500),
    )
    .expect("a valid write lands");
    assert_eq!(report.old_value, None);
    assert_eq!(report.new_value, "250");

    let rows = vike_secrets::read_settings_in(d.path()).expect("reads back");
    assert!(
        rows.rows().is_some_and(|r| r.settings.iter().any(|s| s.section == "policy"
            && s.key == "max_notional_per_order"
            && s.value == "250")),
        "the row must be there"
    );
}

/// The five appearance keys (design system spec §5) are ordinary one-row writes: a word the key
/// knows lands as its JSON string, and a word it does not is refused with the store
/// byte-identical.
#[test]
fn an_appearance_word_lands_and_a_foreign_word_is_refused() {
    let d = dir();
    vike_secrets::plant_settings_rows(
        d.path(),
        &StoredSettings {
            settings: vec![vike_secrets::SettingRow {
                section: "preferences".to_string(),
                key: "log_level".to_string(),
                value: "\"info\"".to_string(),
            }],
            ..Default::default()
        },
    )
    .expect("a fresh store plants");
    let budget = std::time::Duration::from_millis(500);
    let report = write_setting_row(d.path(), "preferences.theme", "midnight", budget)
        .expect("a theme word lands");
    assert_eq!(report.new_value, "\"midnight\"");

    let db = vike_secrets::db_path_in(d.path());
    let before = std::fs::read(&db).expect("the database");
    let err = write_setting_row(d.path(), "preferences.theme", "solarized", budget)
        .expect_err("not a theme");
    assert!(err.to_string().contains("graphite, midnight, dusk, carbon"), "{err}");
    assert_eq!(std::fs::read(&db).expect("the database"), before, "byte-identical");

    write_setting_row(d.path(), "preferences.header_gradient", "true", budget)
        .expect("a boolean lands");
    write_setting_row(d.path(), "preferences.header_gradient", "yes", budget)
        .expect_err("a word is not a boolean");

    // `preferences.vsync` is the same shape: a boolean lands, anything else is refused at the write
    // and leaves the database byte-identical.
    let report =
        write_setting_row(d.path(), "preferences.vsync", "false", budget).expect("a boolean lands");
    assert_eq!(report.new_value, "false");
    let db = vike_secrets::db_path_in(d.path());
    let before = std::fs::read(&db).expect("the database");
    for bad in ["yes", "off", "0", "1", "2.5"] {
        write_setting_row(d.path(), "preferences.vsync", bad, budget)
            .expect_err("only true and false are a vsync");
    }
    assert_eq!(std::fs::read(&db).expect("the database"), before, "byte-identical");
}

/// **The first pass's delay has a ceiling at the WRITE**, through the real validator: the bound
/// itself lands, one millisecond over is refused naming the key and the bound, and the refusal
/// leaves the database byte-identical. Its twin at the LOAD is in
/// `crates/vike-config/tests/mirror.rs` (`a_startup_delay_row_above_the_restore_window_bound_marks_the_load`);
/// both go through `Config::apply`.
#[test]
fn a_startup_delay_above_the_restore_window_bound_is_refused_at_the_write() {
    let d = seeded();
    let budget = std::time::Duration::from_millis(500);
    let bound = vike_venues::venue_restore::MAX_RECONCILE_STARTUP_DELAY_MS;
    let key = "config.reconcile_startup_delay_ms";

    let report = write_setting_row(d.path(), key, &bound.to_string(), budget)
        .expect("the bound itself lands");
    assert_eq!(report.new_value, bound.to_string());

    let db = vike_secrets::db_path_in(d.path());
    let before = std::fs::read(&db).expect("the database");
    let err = write_setting_row(d.path(), key, &(bound + 1).to_string(), budget)
        .expect_err("one millisecond over the bound must not land");
    assert_matches!(err, RowPlanError::Refused(_), "{err:?}");
    let msg = err.to_string();
    assert!(msg.contains("reconcile_startup_delay_ms"), "the refusal names the key: {msg}");
    assert!(msg.contains(&format!("maximum {bound}")), "and the bound: {msg}");
    assert_eq!(std::fs::read(&db).expect("the database"), before, "byte-identical");
}

/// **The bounded client rows are checked at the WRITE, through the real validator.** Each takes its
/// two bounds; one step outside, a negative, a fraction, a word, a quoted number and a boolean are
/// all REFUSED naming the key, and the refusals leave the database byte-identical — no clamp, no
/// coercion of `"7"` into `7`. Its twin at the LOAD is in `crates/vike-config/tests/mirror.rs`
/// (`the_client_tuning_rows_refuse_at_the_load_what_they_refuse_at_the_write`); both go through the
/// same `apply`.
#[test]
fn the_client_tuning_rows_take_their_bounds_and_refuse_everything_else_at_the_write() {
    use crate::config::{MAX_NODE_ACK_WAIT_MS, MIN_NODE_ACK_WAIT_MS};
    use crate::preferences::{
        MAX_AI_TIMEOUT_SECS, MAX_BACKFILL_LOOKBACK_DAYS, MAX_CHART_HISTORY_BARS,
        MAX_OBSERVE_POLL_MS, MIN_AI_TIMEOUT_SECS, MIN_BACKFILL_LOOKBACK_DAYS,
        MIN_CHART_HISTORY_BARS, MIN_OBSERVE_POLL_MS,
    };
    let rows: [(&str, u32, u32); 5] = [
        ("config.node_ack_wait_ms", MIN_NODE_ACK_WAIT_MS, MAX_NODE_ACK_WAIT_MS),
        ("preferences.observe_poll_ms", MIN_OBSERVE_POLL_MS, MAX_OBSERVE_POLL_MS),
        ("preferences.chart_history_bars", MIN_CHART_HISTORY_BARS, MAX_CHART_HISTORY_BARS),
        (
            "preferences.backfill_lookback_days",
            MIN_BACKFILL_LOOKBACK_DAYS,
            MAX_BACKFILL_LOOKBACK_DAYS,
        ),
        ("preferences.ai_timeout_secs", MIN_AI_TIMEOUT_SECS, MAX_AI_TIMEOUT_SECS),
    ];
    let d = seeded();
    let db = vike_secrets::db_path_in(d.path());
    let budget = std::time::Duration::from_millis(500);
    for (key, min, max) in rows {
        let leaf = key.split_once('.').map(|(_, leaf)| leaf).expect("a dotted key");
        for ok in [min, max] {
            let report = write_setting_row(d.path(), key, &ok.to_string(), budget)
                .unwrap_or_else(|e| panic!("{key} = {ok} must land: {e}"));
            assert_eq!(report.new_value, ok.to_string());
        }
        let before = std::fs::read(&db).expect("the database");
        let refused = [
            (min - 1).to_string(),
            (max + 1).to_string(),
            "-1".to_string(),
            "1.5".to_string(),
            "abc".to_string(),
            "\"7\"".to_string(),
            "true".to_string(),
        ];
        for bad in refused {
            let err = write_setting_row(d.path(), key, &bad, budget)
                .expect_err("a value outside the row's range must not land");
            assert_matches!(err, RowPlanError::Refused(_), "{key} = {bad}: {err:?}");
            assert!(err.to_string().contains(leaf), "{key} = {bad}: the refusal names it: {err}");
        }
        assert_eq!(std::fs::read(&db).expect("the database"), before, "{key}: byte-identical");
    }
}

/// The two model-id rows are checked at the write too: a well-shaped id lands as a JSON string, a
/// blank, an over-long and a spaced one are refused, and the store is untouched. (The planner trims
/// the argv value's outer whitespace before it types it, which is why no leading-space case is
/// here: that trim is the writer's, and the loader's own refusal of a padded STORED id is proven
/// beside `check_model_id`.)
#[test]
fn the_model_id_rows_take_a_shaped_id_and_refuse_a_blank_or_spaced_one_at_the_write() {
    let d = seeded();
    let db = vike_secrets::db_path_in(d.path());
    let budget = std::time::Duration::from_millis(500);
    let too_long = "x".repeat(101);
    for key in ["preferences.ai_anthropic_model", "preferences.ai_cerebras_model"] {
        let report = write_setting_row(d.path(), key, "claude-sonnet-4-5", budget)
            .unwrap_or_else(|e| panic!("{key}: {e}"));
        assert_eq!(report.new_value, "\"claude-sonnet-4-5\"");
        let before = std::fs::read(&db).expect("the database");
        for bad in ["", "claude opus", too_long.as_str()] {
            let err = write_setting_row(d.path(), key, bad, budget)
                .expect_err("a malformed model id must not land");
            assert_matches!(err, RowPlanError::Refused(_), "{key} = {bad:?}: {err:?}");
            assert!(err.to_string().contains("model id"), "{key} = {bad:?}: {err}");
        }
        assert_eq!(std::fs::read(&db).expect("the database"), before, "{key}: byte-identical");
    }
}

/// **The link-patience rows are RAISE-ONLY at the WRITE**, through the real validator: the built-in
/// window (the floor) and the ceiling land, one millisecond under the floor and one over the
/// ceiling are refused naming the key and the bound, and each refusal leaves the database
/// byte-identical. Their twin at the LOAD is in `crates/vike-config/tests/mirror.rs`
/// (`a_link_patience_row_outside_its_window_marks_the_load`); both go through `Preferences::apply`.
#[test]
fn a_link_patience_row_outside_its_window_is_refused_at_the_write() {
    use crate::preferences::{
        DEFAULT_CONTROL_REPLY_TIMEOUT_MS, DEFAULT_OBSERVE_STALL_TIMEOUT_MS,
        MAX_CONTROL_REPLY_TIMEOUT_MS, MAX_OBSERVE_STALL_TIMEOUT_MS,
    };
    let budget = std::time::Duration::from_millis(500);
    for (key, floor, max) in [
        (
            "preferences.observe_stall_timeout_ms",
            DEFAULT_OBSERVE_STALL_TIMEOUT_MS,
            MAX_OBSERVE_STALL_TIMEOUT_MS,
        ),
        (
            "preferences.control_reply_timeout_ms",
            DEFAULT_CONTROL_REPLY_TIMEOUT_MS,
            MAX_CONTROL_REPLY_TIMEOUT_MS,
        ),
    ] {
        let d = seeded();
        let db = vike_secrets::db_path_in(d.path());
        for ok in [floor, max] {
            let report = write_setting_row(d.path(), key, &ok.to_string(), budget)
                .unwrap_or_else(|e| panic!("{key} = {ok} must land: {e}"));
            assert_eq!(report.new_value, ok.to_string());
        }
        let bare = key.rsplit('.').next().expect("a dotted key");
        for (bad, bound_word) in [(floor - 1, "minimum"), (max + 1, "maximum")] {
            let before = std::fs::read(&db).expect("the database");
            let err = write_setting_row(d.path(), key, &bad.to_string(), budget)
                .expect_err("outside the window must not land");
            assert_matches!(err, RowPlanError::Refused(_), "{err:?}");
            let msg = err.to_string();
            assert!(msg.contains(bare), "the refusal names the key: {msg}");
            assert!(msg.contains(bound_word), "and says which bound: {msg}");
            assert_eq!(std::fs::read(&db).expect("the database"), before, "byte-identical");
        }
    }
}

/// **The two connection-timing rows are bounded at the WRITE**, through the real validator: each
/// end of the range lands, one millisecond outside either end is refused naming the key and the
/// bound it missed, and every refusal leaves the database byte-identical. The load's twin is in
/// `crates/vike-config/tests/mirror.rs` (`a_connection_timing_row_outside_its_range_marks_the_load`);
/// both go through `Config::apply`.
#[test]
fn a_connection_timing_row_outside_its_range_is_refused_at_the_write() {
    let d = seeded();
    let budget = std::time::Duration::from_millis(500);
    let db = vike_secrets::db_path_in(d.path());

    for (key, min, max) in [
        (
            "config.node_idle_read_timeout_ms",
            crate::config::NODE_IDLE_READ_TIMEOUT_MIN_MS,
            crate::config::NODE_IDLE_READ_TIMEOUT_MAX_MS,
        ),
        (
            "config.md_heartbeat_ms",
            crate::config::MD_HEARTBEAT_MIN_MS,
            crate::config::MD_HEARTBEAT_MAX_MS,
        ),
    ] {
        for end in [min, max] {
            let report = write_setting_row(d.path(), key, &end.to_string(), budget)
                .unwrap_or_else(|e| panic!("{key} = {end}: the end of the range lands: {e}"));
            assert_eq!(report.new_value, end.to_string());
        }

        let before = std::fs::read(&db).expect("the database");
        for (outside, bound_word) in
            [(min - 1, format!("minimum {min}")), (max + 1, format!("maximum {max}"))]
        {
            let err = write_setting_row(d.path(), key, &outside.to_string(), budget)
                .expect_err("one millisecond outside the range must not land");
            assert_matches!(err, RowPlanError::Refused(_), "{key} = {outside}: {err:?}");
            let msg = err.to_string();
            let leaf = key.trim_start_matches("config.");
            assert!(msg.contains(leaf), "the refusal names the key: {msg}");
            assert!(msg.contains(&bound_word), "and the bound it missed: {msg}");
            assert_eq!(std::fs::read(&db).expect("the database"), before, "byte-identical");
        }
    }
}

/// A write whose candidate would resolve a DIFFERENT key than the one named is refused — the
/// collateral-promotion guard, enforced by `differing_keys`.
#[test]
fn a_bad_key_refuses_before_any_database_is_touched() {
    let d = dir();
    let err =
        write_setting_row(d.path(), "confgi.api_key", "x", std::time::Duration::from_millis(500))
            .expect_err("an unknown section must refuse");
    assert_matches!(err, RowPlanError::BadKey { .. }, "{err:?}");
    assert!(!d.path().join("vike.db").exists(), "nothing was created for a refusal");
}

/// A store holding one setting row, so a candidate resolves and an ordinary write can land.
fn seeded() -> tempfile::TempDir {
    let d = dir();
    vike_secrets::plant_settings_rows(
        d.path(),
        &StoredSettings {
            settings: vec![vike_secrets::SettingRow {
                section: "preferences".to_string(),
                key: "log_level".to_string(),
                value: "\"info\"".to_string(),
            }],
            ..Default::default()
        },
    )
    .expect("a fresh store plants");
    d
}

/// **Every credential shape is refused by the PLANNER, before the database is opened** — each row
/// of `crate::redact::SECRET_RULES` in the position its shape matches (`config.acme_x_token_y`,
/// `config.acme_demo_user`) and as the bare word (`preferences.token`). The keys are built from
/// `crate::redact::SECRET_RULES` itself, so a shape added there is covered here unasked.
///
/// "Before the database is opened" is proven two ways: a SEEDED store is byte-identical afterwards,
/// and a directory with NO database answers the credential refusal rather than `NoDatabase`, which
/// it could only do if the refusal ran first.
#[test]
fn every_credential_shape_is_refused_before_the_database_is_opened() {
    use crate::redact::Shape;

    let budget = std::time::Duration::from_millis(500);
    let value = "a-value-nobody-may-store";
    let d = seeded();
    let db = vike_secrets::db_path_in(d.path());
    let before = std::fs::read(&db).expect("the database");
    let empty = dir();

    for rule in crate::redact::SECRET_RULES {
        let lower = rule.needle.to_ascii_lowercase();
        let keys = match rule.shape {
            // The needle in the MIDDLE of a leaf, and the bare word.
            Shape::Anywhere => [
                format!("config.acme_x{lower}_y"),
                format!("preferences.{}", lower.trim_start_matches('_')),
            ],
            // A leaf that ENDS in the shape, and a leaf that IS the bare shape.
            Shape::SuffixOrBare => {
                [format!("config.acme{lower}"), format!("preferences.{}", &lower[1..])]
            }
            Shape::Whole => [format!("config.{lower}"), format!("preferences.{lower}")],
        };
        for key in keys {
            assert_matches!(
                refuse_credential_key(&key),
                Err(RowPlanError::CredentialKey { .. }),
                "{key}"
            );
            let err = write_setting_row(d.path(), &key, value, budget)
                .expect_err("a credential-shaped key must be refused");
            assert_matches!(err, RowPlanError::CredentialKey { .. }, "{key}: {err:?}");
            let msg = err.to_string();
            assert!(msg.contains(&key), "it names the key: {msg}");
            assert!(!msg.contains(value), "it never repeats the value: {msg}");
            assert!(msg.contains("secrets set") && msg.contains("stdin"), "{msg}");
            assert!(
                msg.contains("config show"),
                "a mistyped SETTING is pointed at the list: {msg}"
            );

            let err = write_setting_row(empty.path(), &key, value, budget)
                .expect_err("no database, and a credential-shaped key");
            assert_matches!(
                err,
                RowPlanError::CredentialKey { .. },
                "the refusal must run before the database is looked for: {key}: {err:?}"
            );
        }
    }
    assert_eq!(std::fs::read(&db).expect("the database"), before, "byte-identical");
    assert!(!vike_secrets::db_path_in(empty.path()).exists(), "nothing was created");

    // The anti-vacuity control: the same store takes an ordinary key, so the refusals above are
    // the fence and not a store that refuses everything.
    write_setting_row(d.path(), "config.tradehub_addr", "127.0.0.1:7879", budget)
        .expect("an ordinary settings key lands");
    refuse_credential_key("policy.max_notional_per_order").expect("an ordinary key passes");
}

/// **No settings key is exempt from the credential fence by its shape any more.** The arming
/// exemption (`policy.accounts.<venue>.<LABEL>`, whose leaf could be the label `KEY` or `USER`)
/// went with the arming rows (decision 0119), so such a key meets the fence like any other.
#[test]
fn a_retired_account_key_meets_the_credential_fence() {
    for key in ["policy.accounts.binance.KEY", "policy.accounts.binance.USER", "config.key"] {
        assert!(
            crate::redact::is_secret_key(key),
            "precondition: {key}'s leaf is credential-shaped"
        );
        assert_matches!(
            refuse_credential_key(key),
            Err(RowPlanError::CredentialKey { .. }),
            "{key}"
        );
    }
}

/// **The trading-mount batch's rows take the whole road a row takes**: `config set` writes the row
/// through the real validator, the loader reads it back as the typed value, and `config show`'s
/// description reports the value with the database as its origin. A refused write leaves the store
/// byte-identical, and a word that is not a number is refused naming the key (the type check, not a
/// bound — serde's own message carries no name, `crate::mirror`'s `offending_leaf` restores it).
#[test]
fn a_batch_row_round_trips_through_set_and_show() {
    let budget = std::time::Duration::from_millis(500);
    let d = seeded();
    // (key, the text `config set` is given, the value `config show` renders). The two disk floors
    // go in the order that keeps warn above fail at every step.
    let rows = [
        ("config.paper_slippage", "0.0005", "0.0005"),
        ("config.preflight_disk_warn_mib", "8192", "8192"),
        ("config.preflight_disk_fail_mib", "2048", "2048"),
        ("config.inflight_confirm_ms", "2000", "2000"),
        ("config.strategy_state_save_ms", "60000", "60000"),
    ];
    for (key, raw, shown) in rows {
        let report = write_setting_row(d.path(), key, raw, budget)
            .unwrap_or_else(|e| panic!("{key} = {raw}: {e}"));
        assert_eq!(report.new_value, raw, "{key}: lands as the typed scalar");

        let read = vike_secrets::read_settings_in(d.path());
        let mut refusal = String::new();
        let layer = crate::StoreLayer::of(Some(&read), &mut refusal);
        let described = crate::describe_with_source(Some(d.path()), layer).expect("describes");
        let row = described.rows.iter().find(|r| r.key == key).expect("a key `show` lists");
        assert_eq!(row.value.as_deref(), Some(shown), "{key}: the effective value");
        assert_eq!(row.origin, crate::Origin::Db, "{key}: the row is the origin");
        assert!(described.settings.seal_refusal.is_none(), "{key}: {:?}", described.settings);
    }

    let db = vike_secrets::db_path_in(d.path());
    let before = std::fs::read(&db).expect("the database");
    for (key, leaf) in [
        ("config.paper_slippage", "paper_slippage"),
        ("config.preflight_disk_warn_mib", "preflight_disk_warn_mib"),
        ("config.preflight_disk_fail_mib", "preflight_disk_fail_mib"),
        ("config.inflight_confirm_ms", "inflight_confirm_ms"),
        ("config.strategy_state_save_ms", "strategy_state_save_ms"),
    ] {
        let err =
            write_setting_row(d.path(), key, "plenty", budget).expect_err("a word is not a number");
        assert_matches!(err, RowPlanError::Refused(_), "{key}: {err:?}");
        assert!(err.to_string().contains(leaf), "{key}: the refusal names the key: {err}");
        assert_eq!(std::fs::read(&db).expect("the database"), before, "{key}: byte-identical");
    }

    // The pair, through the real validator: the store holds warn 8192 / fail 2048, so a warn floor
    // at or below the fail floor is refused naming both keys, and a fail floor at or above the warn
    // floor likewise — neither lands.
    for (key, raw) in
        [("config.preflight_disk_warn_mib", "2048"), ("config.preflight_disk_fail_mib", "8192")]
    {
        let err = write_setting_row(d.path(), key, raw, budget)
            .expect_err("a pair that does not order must not land");
        assert_matches!(err, RowPlanError::Refused(_), "{key}: {err:?}");
        let msg = err.to_string();
        assert!(msg.contains("preflight_disk_warn_mib"), "{key}: {msg}");
        assert!(msg.contains("preflight_disk_fail_mib"), "{key}: {msg}");
        assert_eq!(std::fs::read(&db).expect("the database"), before, "{key}: byte-identical");
    }
}

// ---------------------------------------------------------------------------------------------
// `unset_setting_row` — the removal, and the cure for a store the loader refuses (decision 0161)
// ---------------------------------------------------------------------------------------------

fn budget() -> std::time::Duration {
    std::time::Duration::from_millis(500)
}

/// A SEALED store: the planted fixture (one `preferences.log_level` row, no seal) plus one
/// sanctioned write, which is what creates the seal. Two rows, seal at two.
fn sealed() -> tempfile::TempDir {
    let d = seeded();
    write_setting_row(d.path(), "config.tradehub_addr", "127.0.0.1:7879", budget())
        .expect("the first sanctioned write seals the store");
    d
}

/// The rows and the seal of the store in `d`, exactly as a daemon's boot reads them.
fn rows_and_seal(d: &Path) -> (StoredSettings, Option<Adoption>) {
    match vike_secrets::read_settings_in(d).expect("the store reads") {
        vike_secrets::SettingsSource::Rows { rows, adopted } => (rows, adopted),
        other => panic!("expected rows, got {other:?}"),
    }
}

/// What the loader makes of the store in `d` — the mark a daemon would refuse on, or `None`.
fn refusal_of(d: &Path) -> Option<String> {
    let (rows, adopted) = rows_and_seal(d);
    crate::load_with_source(
        None,
        StoreLayer::Rows { rows: &rows, adopted: adopted.as_ref() },
        &CliOverrides::default(),
    )
    .expect("a row problem is a mark, never an Err")
    .seal_refusal
}

/// **Plant `section.key = value` into a SEALED store as ONE more row, moving the seal's count with
/// it** — the state a key that was legal when written and was RETIRED by a later release leaves
/// behind. No sanctioned writer can produce it any more, which is why it is planted through the
/// whole-table replace (`write_settings_in`, which moves the count of a seal that exists).
fn plant_stale_row(d: &Path, section: &str, key: &str, value: &str) {
    let (mut rows, _) = rows_and_seal(d);
    rows.settings.push(vike_secrets::SettingRow {
        section: section.to_string(),
        key: key.to_string(),
        value: value.to_string(),
    });
    vike_secrets::write_settings_in(d, &rows.sorted()).expect("the stale row plants");
}

/// Make the seal's COUNT disagree with the table, by the one route a test can reach without a
/// second SQLite binding: the write lock's own connection runs the statement inside the
/// transaction it already holds, then commits.
fn desync_seal(d: &Path, count: usize) {
    let held = vike_secrets::hold_write_lock(d);
    held.execute_batch(&format!("UPDATE settings_adoption SET setting_rows = {count}; COMMIT;"))
        .expect("the seal's count is rewritten");
}

/// **A known key's removal returns it to its compiled-in default, and the store still boots.** The
/// default comes back as what `config show` would render — a value for `preferences.log_level`,
/// and NO value at all for a ceiling (`policy.max_notional_per_order` is uncapped by default, which
/// the report says rather than hides).
#[test]
fn unsetting_a_known_row_returns_it_to_its_default_and_the_store_still_boots() {
    let d = sealed();
    write_setting_row(d.path(), "preferences.log_level", "debug", budget()).expect("a write");
    write_setting_row(d.path(), "policy.max_notional_per_order", "250", budget()).expect("a write");
    assert_eq!(rows_and_seal(d.path()).1.map(|a| a.setting_rows), Some(3));

    let report = unset_setting_row(d.path(), "preferences.log_level", budget())
        .expect("a known key's row is removable");
    assert_eq!(report.key, "preferences.log_level");
    assert_eq!(report.now, KeyAfterRemoval::Default(Some("info".to_string())));
    assert_eq!(report.seal, Some((3, 2)));
    assert_eq!(report.still_refused, None);

    let report = unset_setting_row(d.path(), "policy.max_notional_per_order", budget())
        .expect("a ceiling's row is removable too");
    assert_eq!(
        report.now,
        KeyAfterRemoval::Default(None),
        "a ceiling's default is NO CAP, and the report must carry that, not a number"
    );
    assert_eq!(report.seal, Some((2, 1)));

    let (rows, seal) = rows_and_seal(d.path());
    assert_eq!(rows.settings.len(), 1, "exactly the two named rows are gone");
    assert_eq!(seal.map(|a| a.setting_rows), Some(1), "the seal moved with the table");
    assert_eq!(refusal_of(d.path()), None, "the store boots clean afterwards");
    let settings = crate::load_with_source(
        None,
        StoreLayer::Rows { rows: &rows, adopted: None },
        &CliOverrides::default(),
    )
    .expect("loads");
    assert_eq!(settings.preferences.log_level, "info", "the key is its default again");
    assert_eq!(settings.policy.max_notional_per_order, None);
}

/// **`policy.link_deadman_grace_ms` is the one key whose unset default is an ARMED protection**, and
/// the dead-man warnings send the operator to this very removal. Its report must say the grace is
/// armed — not "no value", which would read as the switch still being off.
#[test]
fn removing_the_link_grace_row_reports_the_armed_default_not_no_value() {
    let d = sealed();
    write_setting_row(d.path(), "policy.link_deadman_grace_ms", "0", budget())
        .expect("zero switches the connection dead-man off");
    let report = unset_setting_row(d.path(), "policy.link_deadman_grace_ms", budget())
        .expect("the row is removable");
    match report.now {
        KeyAfterRemoval::Default(Some(said)) => {
            assert!(said.contains("ARMED"), "{said}");
            assert!(said.contains(&crate::DEFAULT_LINK_DEADMAN_GRACE_MS.to_string()), "{said}");
        }
        other => panic!("an armed default was reported as {other:?}"),
    }
}

/// **THE BRICK CURE.** A key that was legal when written and was retired later leaves a sealed
/// store the loader refuses — and `config set` refuses to touch it ("the current settings store
/// does not boot clean"), the backup still holds the row, and a hand DELETE trips the count. This
/// is the state the verb exists for: removing the row repairs the store, the count moves with it,
/// and an ordinary write works again.
#[test]
fn unsetting_a_retired_row_repairs_a_store_that_failed_the_seal() {
    let d = sealed();
    plant_stale_row(d.path(), "policy", "max_total_exposure", "500.0");
    let why = refusal_of(d.path()).expect("precondition: the retired row makes the store refuse");
    assert!(why.contains("max_total_exposure"), "{why}");

    // The brick: no `config set` can land, because the validator demands a store that boots clean.
    let before = std::fs::read(vike_secrets::db_path_in(d.path())).expect("the database");
    let err = write_setting_row(d.path(), "preferences.log_level", "warn", budget())
        .expect_err("a write over a refused store is refused");
    assert!(err.to_string().contains("does not boot clean"), "{err}");
    assert_eq!(
        std::fs::read(vike_secrets::db_path_in(d.path())).expect("the database"),
        before,
        "…and changed nothing"
    );

    let report = unset_setting_row(d.path(), "policy.max_total_exposure", budget())
        .expect("the narrow validator lets the repair through");
    assert_eq!(report.now, KeyAfterRemoval::Unknown, "a retired key has no default to return to");
    assert_eq!(report.seal, Some((3, 2)));
    assert_eq!(report.still_refused, None, "nothing else was wrong with the store");

    assert_eq!(refusal_of(d.path()), None, "the store boots clean again");
    let (rows, seal) = rows_and_seal(d.path());
    assert_eq!(seal.map(|a| a.setting_rows), Some(rows.settings.len()), "the count is sound");
    write_setting_row(d.path(), "preferences.log_level", "warn", budget())
        .expect("and an ordinary write lands again");
}

/// Two refused rows need two removals, and the first one SAYS the store still refuses — naming why
/// — so the operator runs the verb again for the other instead of believing the store is fixed.
#[test]
fn unsetting_one_of_two_refused_rows_reports_the_store_still_refuses() {
    let d = sealed();
    plant_stale_row(d.path(), "policy", "max_total_exposure", "500.0");
    plant_stale_row(d.path(), "preferences", "rate_utilization", "0.5");

    let first = unset_setting_row(d.path(), "policy.max_total_exposure", budget())
        .expect("the first removal lands");
    let why = first.still_refused.expect("the other refused row keeps the store refusing");
    assert!(why.contains("rate_utilization"), "it names the OTHER row: {why}");
    assert!(refusal_of(d.path()).is_some());

    let second = unset_setting_row(d.path(), "preferences.rate_utilization", budget())
        .expect("the second removal lands");
    assert_eq!(second.still_refused, None);
    assert_eq!(refusal_of(d.path()), None);
}

/// **A leftover key's value is never echoed.** The row holds something that looks like a pasted
/// credential; neither the report nor any refusal around it may carry it.
#[test]
fn an_unset_never_echoes_the_value_of_the_row_it_removes() {
    let d = sealed();
    let secret = "pasted-credential-hunter2";
    plant_stale_row(d.path(), "config", "pasted_into_the_wrong_place", &format!("\"{secret}\""));
    let report = unset_setting_row(d.path(), "config.pasted_into_the_wrong_place", budget())
        .expect("the row is removable");
    assert!(!format!("{report:?}").contains(secret), "the report carried the value: {report:?}");

    // …and the refusal for a key that is no longer there repeats nothing either.
    let err = unset_setting_row(d.path(), "config.pasted_into_the_wrong_place", budget())
        .expect_err("the row is gone");
    assert!(!err.to_string().contains(secret), "{err}");
}

/// **A credential-shaped key is refused before the database is opened**, for every shape
/// `SECRET_RULES` knows — the same fence `config set` has, because this verb takes a key from the
/// command line too. A directory with NO database answers the credential refusal rather than
/// `NoDatabase`, which it could only do if the refusal ran first.
#[test]
fn a_credential_shaped_key_is_refused_by_unset_before_the_database_is_opened() {
    use crate::redact::Shape;

    let d = sealed();
    let db = vike_secrets::db_path_in(d.path());
    let before = std::fs::read(&db).expect("the database");
    let empty = dir();

    for rule in crate::redact::SECRET_RULES {
        let lower = rule.needle.to_ascii_lowercase();
        let keys = match rule.shape {
            Shape::Anywhere => [
                format!("config.acme_x{lower}_y"),
                format!("preferences.{}", lower.trim_start_matches('_')),
            ],
            Shape::SuffixOrBare => {
                [format!("config.acme{lower}"), format!("preferences.{}", &lower[1..])]
            }
            Shape::Whole => [format!("config.{lower}"), format!("preferences.{lower}")],
        };
        for key in keys {
            let err = unset_setting_row(d.path(), &key, budget())
                .expect_err("a credential-shaped key must be refused");
            assert_matches!(err, RowPlanError::CredentialKey { .. }, "{key}: {err:?}");
            let err = unset_setting_row(empty.path(), &key, budget())
                .expect_err("no database, and a credential-shaped key");
            assert_matches!(
                err,
                RowPlanError::CredentialKey { .. },
                "the refusal must run before the database is looked for: {key}: {err:?}"
            );
        }
    }
    assert_eq!(std::fs::read(&db).expect("the database"), before, "byte-identical");
    assert!(!vike_secrets::db_path_in(empty.path()).exists(), "nothing was created");
}

/// A key that has no row is refused with a clear message, and the store is byte-identical — for an
/// unknown key and for a known one that simply was never written.
#[test]
fn a_key_with_no_row_is_refused_by_name_and_changes_nothing() {
    let d = sealed();
    let db = vike_secrets::db_path_in(d.path());
    let before = std::fs::read(&db).expect("the database");
    for key in ["policy.max_levrage", "flags.reconcile", "policy.rate.max_utilization"] {
        let err =
            unset_setting_row(d.path(), key, budget()).expect_err("there is no such row to remove");
        assert_matches!(
            err,
            RowPlanError::Refused(vike_secrets::RowWriteError::NoSuchRow { .. }),
            "{key}: {err:?}"
        );
        let message = err.to_string();
        assert!(message.contains(key), "it names the key: {message}");
        assert!(message.contains("nothing was removed"), "{message}");
    }
    assert_eq!(std::fs::read(&db).expect("the database"), before, "byte-identical");
}

/// A key outside the four sections — a venue key, a typo'd section, a whole section, an empty
/// segment — is refused as a BAD KEY before any database is looked for.
#[test]
fn a_key_outside_the_four_sections_is_refused_before_the_database_is_opened() {
    let empty = dir();
    for key in ["venue.polymarket.proxy_host", "confgi.tradehub_addr", "policy", "policy.", "x"] {
        let err = unset_setting_row(empty.path(), key, budget())
            .expect_err("a key no section can spell must be refused");
        assert_matches!(err, RowPlanError::BadKey { .. }, "{key}: {err:?}");
    }
    assert!(!vike_secrets::db_path_in(empty.path()).exists(), "nothing was created");
}

/// **The seal's count stays valid through every change a row can go through**, asserted by the
/// loader's own erase detector rather than by a number: a write, a removal, a write.
#[test]
fn the_seal_count_stays_valid_across_a_write_a_removal_and_a_write() {
    let d = sealed();
    let sound = |d: &Path| {
        let (rows, seal) = rows_and_seal(d);
        let seal = seal.expect("a sealed store");
        crate::mirror::adoption_integrity(&rows, &seal)
            .unwrap_or_else(|e| panic!("the seal's count went unsound: {e}"));
        assert_eq!(seal.setting_rows, rows.settings.len());
    };
    sound(d.path());
    write_setting_row(d.path(), "flags.reconcile_off", "true", budget()).expect("a write");
    sound(d.path());
    unset_setting_row(d.path(), "flags.reconcile_off", budget()).expect("a removal");
    sound(d.path());
    unset_setting_row(d.path(), "config.tradehub_addr", budget()).expect("a removal");
    sound(d.path());
    write_setting_row(d.path(), "config.tradehub_addr", "127.0.0.1:7999", budget())
        .expect("a write");
    sound(d.path());
}

/// **A store whose seal COUNT is already off is refused, and nothing is changed**: removing a row
/// would only move a count that is wrong and hide the erase the detector exists to catch. The cure
/// for that cause stays the backup, and the refusal says why.
#[test]
fn an_unset_over_a_seal_count_that_is_off_is_refused_and_changes_nothing() {
    let d = sealed();
    desync_seal(d.path(), 7);
    let db = vike_secrets::db_path_in(d.path());
    let before = std::fs::read(&db).expect("the database");

    let err = unset_setting_row(d.path(), "config.tradehub_addr", budget())
        .expect_err("a removal cannot launder an unsound count");
    assert_matches!(
        err,
        RowPlanError::Refused(vike_secrets::RowWriteError::Rejected(_)),
        "{err:?}"
    );
    let message = err.to_string();
    assert!(message.contains("already unsound"), "{message}");
    assert!(message.contains("nightly backup"), "the cure that is left is named: {message}");
    assert_eq!(std::fs::read(&db).expect("the database"), before, "byte-identical");
}

/// **The only row of a sealed store: the seal goes with it.** A seal over zero rows is the state
/// `vike-cli config check` fails as "rows erased after the seal", and a removal that legitimately
/// emptied the table must not leave that behind — the store is the ordinary empty one again, boots
/// clean, and the next write seals it afresh.
#[test]
fn removing_the_only_row_of_a_sealed_store_leaves_the_ordinary_empty_store() {
    let d = dir();
    vike_secrets::plant_settings_rows(d.path(), &StoredSettings::default())
        .expect("an empty store");
    write_setting_row(d.path(), "config.tradehub_addr", "127.0.0.1:7879", budget())
        .expect("the first write seals the store");
    assert_eq!(rows_and_seal(d.path()).1.map(|a| a.setting_rows), Some(1));

    let report = unset_setting_row(d.path(), "config.tradehub_addr", budget()).expect("removal");
    assert_eq!(report.seal, Some((1, 0)), "the report still says the count went to zero");
    let (rows, seal) = rows_and_seal(d.path());
    assert!(rows.settings.is_empty());
    assert!(seal.is_none(), "a seal over zero rows outlived the last row");
    assert_eq!(refusal_of(d.path()), None);

    write_setting_row(d.path(), "config.tradehub_addr", "127.0.0.1:7999", budget())
        .expect("and the next write seals the store afresh");
    assert_eq!(rows_and_seal(d.path()).1.map(|a| a.setting_rows), Some(1));
}

/// A store that was never sealed stays unsealed: the removal reports no seal move, and does not
/// create one.
#[test]
fn an_unset_on_a_never_sealed_store_creates_no_seal() {
    let d = seeded();
    assert!(rows_and_seal(d.path()).1.is_none());
    let report = unset_setting_row(d.path(), "preferences.log_level", budget())
        .expect("a never-sealed store loses a row too");
    assert_eq!(report.seal, None);
    assert!(rows_and_seal(d.path()).1.is_none(), "a removal sealed a store that had no seal");
}

/// The validator's two checks on a store that boots clean, driven directly (no healthy store can
/// be made to lose a row and fail to boot through the real verb, so the branch is held here): the
/// store without the row must boot clean, and no key but the named one may resolve differently.
#[test]
fn a_removal_from_a_healthy_store_is_held_to_what_a_write_would_be() {
    let row = |section: &str, key: &str, value: &str| vike_secrets::SettingRow {
        section: section.to_string(),
        key: key.to_string(),
        value: value.to_string(),
    };
    let current = StoredSettings {
        settings: vec![
            row("policy", "max_leverage", "2.0"),
            row("preferences", "theme", "\"dusk\""),
        ],
        ..Default::default()
    };

    // The honest candidate: the named row gone, nothing else moved.
    let honest = StoredSettings {
        settings: vec![row("preferences", "theme", "\"dusk\"")],
        ..Default::default()
    };
    validate_row_unset("policy.max_leverage", &current, None, &honest)
        .expect("a removal that changes only its own key passes");

    // A candidate that would not boot (a retired key the removal somehow left behind).
    let broken = StoredSettings {
        settings: vec![row("policy", "max_total_exposure", "500.0")],
        ..Default::default()
    };
    let why = validate_row_unset("policy.max_leverage", &current, None, &broken)
        .expect_err("a candidate that cannot boot is refused");
    assert!(why.contains("unable to boot"), "{why}");

    // A candidate that would ALSO change another key.
    let drifting = StoredSettings { settings: Vec::new(), ..Default::default() };
    let why = validate_row_unset("policy.max_leverage", &current, None, &drifting)
        .expect_err("a removal may change only the key it names");
    assert!(why.contains("preferences.theme"), "{why}");
}
