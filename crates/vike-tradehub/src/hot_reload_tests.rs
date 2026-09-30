use super::*;

/// **Exhaustiveness, both directions**, against the same authority `config show` renders
/// from: every non-policy key `vike_config::provenance::setting_keys` knows has a named
/// [`CLASSIFICATION`] row (a NEW settings key reddens this until classified), and every row
/// names a real key (a DELETED key cannot leave a stale row behind). Policy keys are
/// deliberately absent from the table — the file-level seal is the next test.
#[test]
fn every_settings_key_is_classified_and_no_row_is_stale() {
    let authority: Vec<String> = vike_config::provenance::setting_keys()
        .into_iter()
        .filter(|k| k.section != "policy")
        .map(|k| k.key)
        .collect();
    let rows: Vec<String> =
        CLASSIFICATION.iter().map(|(file, field, _)| row_key(*file, field)).collect();
    for key in &authority {
        assert!(
            rows.contains(key),
            "settings key {key:?} has no CLASSIFICATION row — classify it (HotClass::Restart \
                 is the conservative default; a HotClass::Hot row must carry its written reason)"
        );
    }
    for key in &rows {
        assert!(
            authority.contains(key),
            "CLASSIFICATION row {key:?} names no live settings key — delete the stale row"
        );
    }
    assert_eq!(
        rows.len(),
        authority.len(),
        "one row per key exactly (a duplicate row would shadow nothing but confuse readers)"
    );
}

/// Every HOT row carries a real written reason — the `LIVE_CAPABLE` idiom's teeth. Length-
/// checked the way `vike_config::consumed`'s gate checks `why`: a one-word waved-through
/// reason is the shape being prevented.
#[test]
fn every_hot_row_carries_a_written_reason() {
    for (file, field, class) in CLASSIFICATION {
        if let HotClass::Hot { reason } = class {
            assert!(
                reason.len() >= 80 && !reason.to_ascii_lowercase().contains("todo"),
                "hot key {:?} needs a real written reason (what applies it, and why no state \
                     depends on the boot-time value); got {reason:?}",
                row_key(*file, field)
            );
        }
    }
}

/// **`policy.toml` is sealed against the table itself**: [`classify`] answers `Restart` for a
/// policy key even when a (hypothetical, wrong) table row would say otherwise — the file is
/// refused before the table is consulted, so the doctrine cannot be undone by one row.
#[test]
fn a_policy_key_is_never_hot() {
    // ⚠ The policy keys are DERIVED from the authority, never spelled here — two reasons, and
    // the second is why this test reads the way it does. (1) EVERY policy key is covered, not
    // two hand-picked ones, so a new ceiling is sealed the day it is added. (2) A literal
    // `policy.<field>` in this file is READ AS A CONSUMER by
    // `crates/vike-config/tests/policy_is_consumed.rs`, whose scanner greps the tree for a
    // field name and promotes a `Consumed::No` row when it finds one — a test asserting that
    // a ceiling is NOT applied would have been recorded as the code that applies it (measured
    // on the CI box: `Policy::max_leverage is marked Consumed::No, but hot_reload.rs reads it`).
    let policy_keys: Vec<String> = vike_config::provenance::setting_keys()
        .into_iter()
        .filter(|k| k.section == "policy")
        .map(|k| k.key)
        .collect();
    assert!(!policy_keys.is_empty(), "the authority must know some policy keys");
    for key in &policy_keys {
        assert_eq!(
            classify(SettingsFile::Policy, key),
            HotClass::Restart,
            "{key} must be restart-only: policy is sealed (decision 0005)"
        );
    }
    // ...and no policy key has a table row at all, so the seal is not merely shadowing one.
    assert!(
        CLASSIFICATION.iter().all(|(f, _, _)| *f != SettingsFile::Policy),
        "the sealed-policy doctrine forbids policy rows in CLASSIFICATION"
    );
}

/// **The v2 hot set is EXACTLY the two log levels** — a pin, so growing the set is a
/// deliberate diff on this line plus a reasoned table row, never a drive-by.
#[test]
fn the_hot_set_is_exactly_the_two_log_levels() {
    let hot: Vec<String> = CLASSIFICATION
        .iter()
        .filter(|(_, _, c)| matches!(c, HotClass::Hot { .. }))
        .map(|(f, field, _)| row_key(*f, field))
        .collect();
    assert_eq!(hot, ["preferences.log_level", "preferences.log_file_level"]);
}

/// The classify dispatch: a table row answers for its own key; an unknown key (or a key from
/// the wrong file) is conservatively restart-required.
#[test]
fn classify_answers_the_table_and_defaults_to_restart() {
    assert!(matches!(
        classify(SettingsFile::Preferences, "preferences.log_level"),
        HotClass::Hot { .. }
    ));
    assert_eq!(classify(SettingsFile::Config, "config.tradehub_addr"), HotClass::Restart);
    assert_eq!(classify(SettingsFile::Preferences, "preferences.no_such_key"), HotClass::Restart);
}

/// The seam round-trip: a hot request from one thread is applied by a drain on another, the
/// waiter sees `true`; a FAILED apply reports `false`; with NOBODY draining, the deadline
/// answers `false` (the honest restart-required); with the ticker DROPPED (shutdown), the
/// request answers `false` immediately.
#[test]
fn the_apply_seam_reports_executed_failed_timeout_and_gone() {
    // executed
    let (handle, ticker) = hot_apply_channel();
    let waiter = {
        let handle = handle.clone();
        std::thread::spawn(move || {
            handle.request_apply("preferences.log_level", Duration::from_secs(5))
        })
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut applied_keys: Vec<String> = Vec::new();
    while applied_keys.is_empty() && std::time::Instant::now() < deadline {
        ticker.drain(|key| {
            applied_keys.push(key.to_string());
            true
        });
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(waiter.join().expect("waiter thread"), "an executed apply answers true");
    assert_eq!(applied_keys, ["preferences.log_level"], "the tick saw the requested key");

    // failed apply
    let waiter = {
        let handle = handle.clone();
        std::thread::spawn(move || {
            handle.request_apply("preferences.log_level", Duration::from_secs(5))
        })
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut drained = false;
    while !drained && std::time::Instant::now() < deadline {
        ticker.drain(|_| {
            drained = true;
            false
        });
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!waiter.join().expect("waiter thread"), "a failed apply answers false");

    // timeout: nobody drains
    assert!(
        !handle.request_apply("preferences.log_level", Duration::from_millis(50)),
        "an undrained request times out to false (restart-required, honestly)"
    );

    // gone: ticker dropped
    drop(ticker);
    assert!(
        !handle.request_apply("preferences.log_level", Duration::from_millis(50)),
        "a dropped ticker (shutdown) answers false immediately"
    );
}
