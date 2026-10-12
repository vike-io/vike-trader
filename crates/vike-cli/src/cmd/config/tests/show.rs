//! `config show`'s env half: precedence, the READS qualifier, redaction, filters and flags.

use super::*;
use vike_config::is_secret_key;

/// The same fixture read through a caller-supplied MAP rather than `env::var`.
fn injected_row(name: &'static str, default: &'static str) -> Setting {
    Setting { naming: Naming::MapLookup, ..row(name, default) }
}

// -- precedence ----------------------------------------------------------------------------

#[test]
fn an_unset_setting_reports_its_documented_default() {
    let r = resolve(&row("ACME_HOST", "127.0.0.1"), &map(&[]), &map(&[]), &database());
    assert_eq!(r.source, Source::Default);
    assert_eq!(r.value, "127.0.0.1");
    assert_eq!(r.default, "127.0.0.1");
    assert!(!r.secret);
}

#[test]
fn the_store_beats_the_default() {
    let r = resolve(
        &row("ACME_HOST", "127.0.0.1"),
        &map(&[]),
        &map(&[("ACME_HOST", "the CI box")]),
        &database(),
    );
    assert_eq!(r.source, Source::Database);
    assert_eq!(r.value, "the CI box");
    // the DEFAULT column keeps reporting the documented default, not the winning value
    assert_eq!(r.default, "127.0.0.1");
}

#[test]
fn the_process_env_beats_both() {
    let r = resolve(
        &row("ACME_HOST", "127.0.0.1"),
        &map(&[("ACME_HOST", "from-env")]),
        &map(&[("ACME_HOST", "from-the-store")]),
        &database(),
    );
    assert_eq!(r.source, Source::Env);
    assert_eq!(r.value, "from-env");
}

/// PRESENCE wins, not non-emptiness: an exported `NAME=` is a real override and must not
/// silently fall through to the store or the default, because that is exactly the confusion
/// this command exists to end.
#[test]
fn an_empty_override_is_still_an_override() {
    let r = resolve(
        &row("ACME_HOST", "127.0.0.1"),
        &map(&[("ACME_HOST", "")]),
        &map(&[("ACME_HOST", "from-the-store")]),
        &database(),
    );
    assert_eq!(r.source, Source::Env);
    assert_eq!(r.value, "");
}

#[test]
fn the_source_words_are_pinned() {
    assert_eq!(Source::Default.as_str(), "default");
    assert_eq!(Source::Env.as_str(), "env");
    // The fourth word. It is `vike-cli secrets list --json`'s spelling for the same store, so
    // the two disclosure surfaces name one artifact one way.
    assert_eq!(Source::Database.as_str(), "database");
}

/// **THE FIX.** The same store map, the same registry row, the same value — and the SOURCE
/// word follows the store that actually answered, because it is a parameter rather than an
/// assumption.
///
/// A word naming a store nothing reads would point the operator at the wrong artifact: positive
/// confirmation of something false, which is the exact failure
/// `docs/decisions/0054-settings-move-into-one-database.md`'s constraint 2 names.
#[test]
fn the_source_word_names_the_store_that_answered() {
    let (row, store) = (row("ACME_HOST", "127.0.0.1"), map(&[("ACME_HOST", "the CI box")]));

    let migrated = resolve(&row, &map(&[]), &store, &database());
    assert_eq!(migrated.source, Source::Database);
    assert_eq!(migrated.value, "the CI box");
}

/// The precedence is untouched by which store answers: the process environment still wins, and
/// an absent key still falls to its documented default rather than to the other store. That
/// second half is the per-KEY ladder `docs/decisions/0051` forbids, and this is the assertion
/// that it was not smuggled in with the fourth word.
#[test]
fn the_backend_moves_the_word_and_never_the_precedence() {
    let migrated = database();
    let r = resolve(
        &row("ACME_HOST", "127.0.0.1"),
        &map(&[("ACME_HOST", "from-env")]),
        &map(&[("ACME_HOST", "from-the-database")]),
        &migrated,
    );
    assert_eq!(r.source, Source::Env, "the process env still outranks the store");
    assert_eq!(r.value, "from-env");

    let unset = resolve(&row("ACME_HOST", "127.0.0.1"), &map(&[]), &map(&[]), &migrated);
    assert_eq!(unset.source, Source::Default);
    assert_eq!(unset.value, "127.0.0.1");
}

/// The stranded-row diagnosis is about a value not being EXPORTED, so it holds for both stores.
/// It was keyed on `Source::Database` alone, which would have silently stopped firing on every
/// migrated box — a diagnosis that goes quiet exactly where the store moved under the reader.
#[test]
fn the_stranded_flag_survives_the_database() {
    let r = resolve(
        &row("ACME_CONTROL_KEY", ""),
        &map(&[]),
        &map(&[("ACME_CONTROL_KEY", "k")]),
        &database(),
    );
    assert_eq!(r.source, Source::Database);
    assert_eq!(r.reads, Reads::ProcessEnv);
    assert!(r.store_may_not_reach_reader());
}

// -- the READS qualifier -------------------------------------------------------------------

#[test]
fn the_reads_words_are_pinned_and_come_from_naming() {
    assert_eq!(Reads::of(Naming::Literal), Reads::ProcessEnv);
    assert_eq!(Reads::of(Naming::Konst("ACME_ENV")), Reads::ProcessEnv);
    assert_eq!(Reads::of(Naming::MapLookup), Reads::CallerMap);
    assert_eq!(Reads::of(Naming::Dynamic), Reads::Unknown);

    assert_eq!(Reads::ProcessEnv.as_str(), "env");
    assert_eq!(Reads::CallerMap.as_str(), "caller-map");
    assert_eq!(Reads::Unknown.as_str(), "unknown");
}

/// **The provenance fix.** A key that exists ONLY in the credential store, read by a crate that
/// calls `env::var`, is flagged — the real `VIKE_TRADEHUB_CONTROL_KEY` / `vike-cli` shape, where
/// a bare source word would assert a source never consulted.
#[test]
fn a_store_only_value_read_with_env_var_is_flagged() {
    let r = resolve(
        &row("ACME_CONTROL_KEY", ""),
        &map(&[]),
        &map(&[("ACME_CONTROL_KEY", "k")]),
        &database(),
    );
    assert_eq!(r.source, Source::Database, "the store DOES hold it — that stays true");
    assert_eq!(r.reads, Reads::ProcessEnv);
    assert!(r.store_may_not_reach_reader());
}

/// ...and the three shapes that are NOT that failure stay unflagged: a map-reading row (the
/// caller may well hand it the store), an exported value, and an unset one.
#[test]
fn the_flag_is_narrow() {
    let mapped = resolve(
        &injected_row("ACME_HOST", ""),
        &map(&[]),
        &map(&[("ACME_HOST", "x")]),
        &database(),
    );
    assert_eq!(mapped.reads, Reads::CallerMap);
    assert!(!mapped.store_may_not_reach_reader());

    let exported =
        resolve(&row("ACME_HOST", ""), &map(&[("ACME_HOST", "x")]), &map(&[]), &database());
    assert!(!exported.store_may_not_reach_reader());

    let unset = resolve(&row("ACME_HOST", ""), &map(&[]), &map(&[]), &database());
    assert!(!unset.store_may_not_reach_reader());
}

/// The REAL registry has rows of this shape — the finding that motivated the column. Keyed on
/// the property rather than on a name, so it stays true as the registry moves.
#[test]
fn the_real_registry_has_direct_env_readers_a_store_value_would_not_reach() {
    let store_only: Vec<&Setting> =
        all_settings().filter(|s| Reads::of(s.naming) == Reads::ProcessEnv).collect();
    assert!(
        !store_only.is_empty(),
        "no row reads with env::var — the READS column would be decorative"
    );
    let name = store_only[0].name;
    let r = resolve(store_only[0], &map(&[]), &map(&[(name, "x")]), &database());
    assert!(r.store_may_not_reach_reader());
}

// -- redaction -----------------------------------------------------------------------------

#[test]
fn every_required_credential_shape_is_secret() {
    for name in [
        "ACME_API_KEY",
        "ACME_API_SECRET",
        "ACME_API_PASSPHRASE",
        "ACME_BOT_TOKEN",
        "ACME_PRIVATE_KEY",
        "ACME_DEMO1_LOGIN",
        "ACME_DEMO1_PASSWORD",
    ] {
        assert!(is_secret(name), "{name} must be treated as a secret");
    }
}

#[test]
fn the_widened_shapes_are_secret_too() {
    for name in [
        "ACME_CONTROL_KEY",   // the HMAC node keys
        "ACME_CLIENT_SECRET", // OAuth2 secrets not spelled _API_SECRET
        "ACME_PASSPHRASE",
        "ACME_DEMO_USER",
        "ACME_SIGNATURE",
    ] {
        assert!(is_secret(name), "{name} must be treated as a secret");
    }
    // and a bare name spelled exactly like a shape
    assert!(is_secret("PASSWORD"));
    assert!(is_secret("TOKEN"));
}

#[test]
fn ordinary_knobs_are_not_secret() {
    for name in [
        "ACME_HOST",
        "ACME_LOG_DIR",
        "ACME_HOLD_TOKENS",             // plural — not `_TOKEN`
        "ACME_ALLOW_WITHDRAW_KEYS",     // plural — not `_KEY`
        "ACME_SIGNATURE_TYPE",          // a mode, not the signature
        "ACME_RELAYER_API_KEY_ADDRESS", // an address, not the key
        "USERPROFILE",                  // OS path, not a `_USER` credential
    ] {
        assert!(!is_secret(name), "{name} must NOT be redacted");
    }
}

/// The files half is redacted by the same shapes, applied to a dotted key's LEAF — insurance
/// against a credential-shaped settings field being added later. The last assertion states the
/// property that makes it insurance rather than dead code TODAY.
#[test]
fn a_dotted_key_is_redacted_by_its_leaf_segment() {
    assert!(is_secret_key("config.bot_token"));
    assert!(is_secret_key("preferences.client_secret"));
    assert!(!is_secret_key("config.log_dir"));
    assert!(!is_secret_key("policy.max_notional_per_order"));

    let d = vike_config::describe(None).unwrap();
    assert!(d.rows.iter().all(|r| !is_secret_key(&r.key)), "a settings field is secret-shaped");
}

/// The whole point: no store byte reaches [`Resolved`] for a secret row — so neither printer
/// can leak it, whichever store won.
#[test]
fn a_secret_value_never_enters_the_resolved_row() {
    const LEAK: &str = "sk-do-not-print-me";
    for (env, stored) in
        [(map(&[("ACME_API_KEY", LEAK)]), map(&[])), (map(&[]), map(&[("ACME_API_KEY", LEAK)]))]
    {
        let r = resolve(&row("ACME_API_KEY", ""), &env, &stored, &database());
        assert_eq!(r.value, SET);
        assert!(r.secret);
        assert!(!format!("{r:?}").contains(LEAK), "the secret leaked into {r:?}");
    }
}

// The FILES-half twin of the test above (`a_secret_settings_value_never_enters_the_file_row`)
// MOVED to `vike_config::show` with the builder it pins.

/// A redacted row must still report WHERE its value came from — the store vs a shell export is
/// the provenance question, and the answer discloses nothing.
#[test]
fn a_redacted_row_still_reports_its_true_source() {
    let secret = row("ACME_API_KEY", "");
    assert_eq!(
        resolve(&secret, &map(&[("ACME_API_KEY", "k")]), &map(&[]), &database()).source,
        Source::Env
    );
    assert_eq!(
        resolve(&secret, &map(&[]), &map(&[("ACME_API_KEY", "k")]), &database()).source,
        Source::Database
    );
    assert_eq!(resolve(&secret, &map(&[]), &map(&[]), &database()).source, Source::Default);
}

#[test]
fn an_unconfigured_or_empty_secret_prints_unset() {
    // nothing configured it
    assert_eq!(resolve(&row("ACME_API_KEY", ""), &map(&[]), &map(&[]), &database()).value, UNSET);
    // present but empty — configured, but there is no key there to call `<set>`
    let r =
        resolve(&row("ACME_API_KEY", ""), &map(&[("ACME_API_KEY", "")]), &map(&[]), &database());
    assert_eq!(r.value, UNSET);
    assert_eq!(r.source, Source::Env, "still reports that env held the key");
}

/// No credential row today declares a non-empty default; if one ever did, the DEFAULT column
/// must not become the leak the VALUE column is not.
#[test]
fn a_nonempty_secret_default_is_redacted_too() {
    let r = resolve(&row("ACME_API_KEY", "hardcoded"), &map(&[]), &map(&[]), &database());
    assert_eq!(r.default, REDACTED);
    assert!(!format!("{r:?}").contains("hardcoded"));
}

/// The redaction shapes are checked against the REAL registry, not only invented fixtures: a
/// credential row added tomorrow whose name these shapes miss fails HERE instead of surfacing
/// in a pasted issue. Keyed on the substrings that mark a credential rather than on the shape
/// list itself, so the test cannot pass by agreeing with the code it is checking.
#[test]
fn no_settings_row_leaks() {
    const CREDENTIAL_MARKERS: &[&str] =
        &["API_KEY", "SECRET", "PASSWORD", "PASSPHRASE", "PRIVATE_KEY", "TOKEN", "LOGIN"];
    let missed: Vec<&str> = all_settings()
        .map(|s| s.name)
        // A plural form is a collection of ids, not a credential (`..._TOKENS`), so the
        // markers are matched as a SUFFIX and those rows are correctly not flagged.
        .filter(|&name| CREDENTIAL_MARKERS.iter().any(|&m| name.ends_with(m)) && !is_secret(name))
        .collect();
    assert!(missed.is_empty(), "credential-shaped rows that would print in the clear: {missed:?}");
}

// -- the view filters ----------------------------------------------------------------------

#[test]
fn changed_only_drops_every_defaulted_row() {
    let env = map(&[]);
    let stored = map(&[]);
    let all = resolve_all(&env, &stored, &database(), None, false);
    assert_eq!(all.len(), all_settings().count(), "one row per (name, krate) pair");
    // With both stores empty every row falls through to its default, so the "what have I
    // configured?" view is empty.
    assert!(resolve_all(&env, &stored, &database(), None, true).is_empty());
}

#[test]
fn changed_only_keeps_the_rows_a_store_actually_set() {
    let name = all_settings().next().expect("the registry is not empty").name;
    let env = map(&[(name, "x")]);
    let rows = resolve_all(&env, &map(&[]), &database(), None, true);
    assert!(!rows.is_empty());
    assert!(rows.iter().all(|r| r.source == Source::Env));
    assert!(rows.iter().all(|r| r.name == name), "only the configured name survives");
}

#[test]
fn the_filter_matches_name_or_crate_case_insensitively() {
    let (env, stored) = (map(&[]), map(&[]));
    let by_crate = resolve_all(&env, &stored, &database(), Some("VIKE-CLI"), false);
    assert!(by_crate.iter().all(|r| r.krate.contains("vike-cli")));

    let name = all_settings().next().expect("the registry is not empty").name;
    let by_name = resolve_all(&env, &stored, &database(), Some(&name.to_ascii_lowercase()), false);
    assert!(by_name.iter().any(|r| r.name == name));

    assert!(
        resolve_all(&env, &stored, &database(), Some("no-such-setting-anywhere"), false).is_empty()
    );
}

#[test]
fn rows_are_sorted_by_name_then_crate() {
    let rows = resolve_all(&map(&[]), &map(&[]), &database(), None, false);
    let keys: Vec<(&str, &str)> = rows.iter().map(|r| (r.name, r.krate)).collect();
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    assert_eq!(keys, sorted);
}

// -- the FILES half ------------------------------------------------------------------------
// MOVED to `vike_config::show` with the builder (`the_files_half_covers_every_typed_setting`,
// `the_files_filter_matches_the_key_or_the_file`,
// `an_environment_override_is_reported_with_its_variable`) — the tests travel with the code
// they pin, and this module keeps only what still lives here: the env half + the printers'
// glue.

// -- arg parsing ---------------------------------------------------------------------------

#[test]
fn flags_parse_in_both_forms_and_default_to_the_full_human_output() {
    assert_eq!(parse_args(std::iter::empty()).unwrap(), Args::default());
    assert_eq!(Args::default().section, Section::All);

    let a = parse_args(
        ["--json", "--changed-only", "--filter", "recon", "--section", "files"]
            .map(String::from)
            .into_iter(),
    )
    .unwrap();
    assert!(a.json && a.changed_only);
    assert_eq!(a.filter.as_deref(), Some("recon"));
    assert_eq!(a.section, Section::Files);

    let a =
        parse_args(["--filter=poly".to_string(), "--section=env".to_string()].into_iter()).unwrap();
    assert_eq!(a.filter.as_deref(), Some("poly"));
    assert_eq!(a.section, Section::Env);
}

#[test]
fn the_section_flag_picks_halves() {
    assert!(Section::All.files() && Section::All.env());
    assert!(Section::Files.files() && !Section::Files.env());
    assert!(!Section::Env.files() && Section::Env.env());
}

#[test]
fn a_bad_flag_is_a_clean_error() {
    assert!(parse_args(["--nope".to_string()].into_iter()).unwrap_err().contains("--nope"));
    assert!(
        parse_args(["--filter".to_string()].into_iter()).unwrap_err().contains("requires a value")
    );
    assert!(
        parse_args(["--json=1".to_string()].into_iter()).unwrap_err().contains("takes no value")
    );
    assert!(
        parse_args(["--section=nope".to_string()].into_iter())
            .unwrap_err()
            .contains("files|env|all")
    );
    assert_eq!(parse_args(["-h".to_string()].into_iter()).unwrap_err(), "help requested");
}
