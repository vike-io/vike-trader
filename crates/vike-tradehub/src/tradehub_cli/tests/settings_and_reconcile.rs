//! Loader warnings, and the reconcile-on-restart gate and policy.

use super::*;

/// A loader warning is SURFACED, never swallowed. `main` emits exactly what
/// [`settings_warning_lines`] returns, so proving a warning survives that step proves the
/// daemon logs it — the failure this guards is the loader resolving something the operator did
/// not write while they believe their own file is in force.
///
/// ⚠ The warning is HAND-STUFFED, and stays so on purpose. The loader HAS a producer again
/// (`vike_config::NO_SETTINGS_DIRECTORY_WARNING`), and driving this through it would test the
/// producer rather than this daemon's half — the `settings: ` prefix and the verbatim text
/// reaching the log. `an_absent_policy_file_is_the_start_default_and_arms_no_venue` above is
/// where the real producer is asserted at this daemon's edge; this stays a text that no
/// producer emits, so it cannot go green on a coincidence of wording.
#[test]
fn a_loader_warning_is_surfaced_not_swallowed() {
    let mut settings = vike_config::Settings::default();
    settings.warnings.push("preferences.something was resolved to 0.5".to_string());

    let lines = settings_warning_lines(&settings);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with("settings: "), "{}", lines[0]);
    assert!(lines[0].contains("preferences.something"), "names the key: {}", lines[0]);
    assert!(lines[0].contains("0.5"), "carried verbatim, not summarised: {}", lines[0]);
}

/// The quiet path stays quiet: nothing to resolve ⇒ nothing emitted, so a warn line in a
/// daemon's log always means something actually happened.
#[test]
fn a_clean_load_emits_no_settings_lines() {
    assert!(settings_warning_lines(&vike_config::Settings::default()).is_empty());
}

// -- reconcile-on-restart: the S2 default-ON gate and its refusal -------------------------

/// A `flags` settings row for one field, `true`.
fn flag_row(field: &str) -> vike_secrets::StoredSettings {
    vike_secrets::StoredSettings {
        settings: vec![vike_secrets::SettingRow {
            section: "flags".to_string(),
            key: field.to_string(),
            value: "true".to_string(),
        }],
        ..Default::default()
    }
}

/// [`vike_config::load_with_source`] over `rows` in a throwaway settings directory.
fn load_rows(rows: &vike_secrets::StoredSettings) -> vike_config::Settings {
    let dir = tempfile::tempdir().expect("temp settings dir");
    vike_config::load_with_source(
        Some(dir.path()),
        vike_config::StoreLayer::Rows { rows, adopted: None },
        &vike_config::CliOverrides::default(),
    )
    .unwrap()
}

/// The reconcile FORCE-ON flag reads the FLAGS ROW, and the row is its only source.
///
/// ⚠ **What this flag MEANS changed with S2.** `flags.reconcile` is no longer the default answer: a
/// mount that arms a live venue account reconciles without it (see the composed test below). What
/// it still does is force the driver on where the armed-live probe reports nothing, and that is
/// what a `flags` row has to keep being able to do.
///
/// Driven through the REAL `vike_config::load_with_source` rather than a hand-built `Flags`,
/// because the thing being asserted is the LOADER's resolution. The variable that used to override
/// the row refuses startup now (decision 0111), and that refusal is asserted here too.
#[test]
fn the_reconcile_gate_reads_the_flags_row() {
    // Neither flag set by default: the gate then rests entirely on the armed-live probe.
    let bare = vike_config::load(None).unwrap();
    assert!(!bare.flags.reconcile, "unset ⇒ no FORCE-on");
    assert!(!bare.flags.reconcile_off, "unset ⇒ not refused — `false` is the guarded state");

    // The ROW arms it.
    assert!(load_rows(&flag_row("reconcile")).flags.reconcile, "a row must arm the force-on gate");

    // …and the variable that used to outrank the row refuses startup instead.
    let on = HashMap::from([(vike_config::flags::RECONCILE_ENV.to_string(), "1".to_string())]);
    assert!(vike_config::refuse_removed_env(&on).is_err(), "VIKE_RECONCILE must refuse startup");
}

/// The REFUSAL is a written `flags.reconcile_off` row — durable, and the one layer an operator
/// has (decision 0111: the variable that took it on or off for one run refuses startup).
#[test]
fn the_reconcile_refusal_is_the_row() {
    assert!(load_rows(&flag_row("reconcile_off")).flags.reconcile_off);
    let env = HashMap::from([(vike_config::flags::RECONCILE_OFF_ENV.to_string(), "1".to_string())]);
    assert!(
        vike_config::refuse_removed_env(&env).is_err(),
        "VIKE_RECONCILE_OFF must refuse startup"
    );
}

/// **THE S2 property at this daemon's own edge**, composed over the REAL probe this mount uses
/// (`vike_tradehub_core::armed_live_venues`) rather than a hand-picked count: a mount that arms a venue
/// account reconciles with NOTHING set, and a mount that arms none does not. The two halves
/// share one credential set and differ only in the `account` row (decision 0119: an account trades
/// at its own tier while active), which is the lever an operator actually has.
///
/// The credentials are FIELDS on account row 1 (decision 0156 item 6), never a composed name.
#[test]
fn a_live_armed_start_reconciles_with_nothing_set_and_a_paper_one_does_not() {
    let venue = "bybit";
    let pair = [("API_KEY", "k"), ("API_SECRET", "s")];
    let creds: HashMap<String, String> = HashMap::new();

    // NO account row — an unread table, the default. Credentials present and ignored.
    let mut book = CredentialBook::new();
    for (field, value) in pair {
        book.insert_account(1, field, value);
    }
    let unread = vike_tradehub_core::AdapterPolicy::default();
    let paper = vike_tradehub_core::AdapterPolicy {
        accounts: unread.accounts.clone().with_credentials(book),
        ..unread
    };
    let none_armed = crate::scripted_arming_tests::armed_live_venues(&creds, &paper);
    assert!(none_armed.is_empty(), "no account row arms nothing: {none_armed:?}");
    assert!(
        !reconcile_config::reconcile_gate(false, false, none_armed.len()).enabled(),
        "a PAPER mount must build no reconcile driver — that is what keeps the default from \
             meaning `every process now talks to a venue`"
    );

    // …the same credentials on one ACTIVE `demo` row for the venue's default account.
    let armed_policy = policy_holding(venue, vike_model::AccountTier::Demo, &pair);
    let armed = crate::scripted_arming_tests::armed_live_venues(&creds, &armed_policy);
    assert!(armed.contains(&venue.to_string()), "the active demo row must arm {venue}: {armed:?}");
    let gate = reconcile_config::reconcile_gate(false, false, armed.len());
    assert_eq!(gate, reconcile_config::ReconcileGate::LiveDefault);
    assert!(gate.enabled(), "an armed account reconciles with NOTHING set — the S2 default");

    // …and the operator can still refuse it without disarming the venue.
    assert!(!reconcile_config::reconcile_gate(false, true, armed.len()).enabled());
}

/// QUARANTINE-FIRST: with no `config.reconcile_policy` row the daemon runs `quarantine`, so even
/// a local-origin divergence (`MissingFill`) is HELD — a default-on live daemon auto-folds NOTHING
/// (CLAUDE.md's rule: `hybrid` auto-applies `PositionDrift` on an incomplete position fetch,
/// rewriting position size and booking realized PnL at the venue's price).
///
/// ⚠ The default lives in `crate::reconcile_config::parse_policy`. This test stays HERE because
/// the claim it makes is about THIS DAEMON's effective policy — the thing an operator reads off
/// `docs/ops/tradehub-the CI box.md` — not about the helper.
#[test]
fn daemon_reconcile_policy_defaults_to_quarantine() {
    use vike_exec::recon::{DivergenceKind, ReconMode};
    let recon =
        daemon_recon_settings(vike_config::Flags::default(), &vike_config::Config::default());
    let cfg = reconcile_config::build_recon_config(&recon, HashMap::new());
    assert_eq!(cfg.policy.default, ReconMode::Quarantine);
    assert_eq!(cfg.policy.mode_for(DivergenceKind::MissingFill), ReconMode::Quarantine);
    assert!(
        crate::reconcile_config::auto_applied_kinds(&cfg.policy).is_empty(),
        "the daemon's own default must fold NOTHING without an operator"
    );
}

/// An operator who writes `config.reconcile_policy = hybrid` is honored verbatim — the
/// quarantine-first default only fills an ABSENT row.
#[test]
fn daemon_reconcile_policy_honors_the_row() {
    use vike_exec::recon::{DivergenceKind, ReconMode};
    let config = vike_config::Config {
        reconcile_policy: Some("hybrid".to_string()),
        ..vike_config::Config::default()
    };
    let recon = daemon_recon_settings(vike_config::Flags::default(), &config);
    let cfg = reconcile_config::build_recon_config(&recon, HashMap::new());
    // hybrid auto-synthesizes a local-origin MissingFill (the quarantine default holds it).
    assert_eq!(cfg.policy.mode_for(DivergenceKind::MissingFill), ReconMode::Synthesize);
}

// -- the boot map holds no venue value (decision 0146's second reopener) ------------------------

/// A project holding BOTH kinds of credential row: the deployment rows this daemon's consumers read
/// by name (the Telegram token and ids, the alert webhooks) and the venue-owned ones it must never
/// hold (two ACCOUNTS' fields, a `live` one among them, and a venue's APPLICATION field). Holds the
/// temp dir, the environment naming it, the non-venue `(name, value)` rows and every venue
/// VALUE planted.
struct BothKinds {
    _dir: tempfile::TempDir,
    env: HashMap<String, String>,
    deployment: Vec<(String, String)>,
    venue_values: Vec<String>,
}

fn project_holding_both_kinds() -> BothKinds {
    let dir = tempfile::tempdir().expect("tempdir");
    let settings = dir.path().join("settings");
    std::fs::create_dir_all(&settings).expect("settings dir");
    vike_secrets::create_store(settings.to_str()).expect("create the empty store");
    let deployment: Vec<(String, String)> = [
        ("VIKE_TELEGRAM_BOT_TOKEN", "123456:telegram-token"),
        ("VIKE_TELEGRAM_ALLOWED_CHAT_IDS", "42,43"),
        ("VIKE_TELEGRAM_ALLOWED_USER_IDS", "7"),
        ("VIKE_ALERT_WEBHOOK_URL", "https://hooks.example/alerts"),
        ("VIKE_ALERT_TELEGRAM_TOKEN", "654321:alert-token"),
        ("VIKE_ALERT_TELEGRAM_CHAT_ID", "9001"),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
    .collect();
    vike_secrets::save_credentials_to_store_unjournalled(
        &settings,
        vike_secrets::Table::Credential,
        &deployment,
        vike_secrets::UpsertIntent::Add,
    )
    .expect("seed the deployment rows");

    let mut venue_values = Vec::new();
    for (venue, tier) in
        [("oanda", vike_model::AccountTier::Demo), ("binance", vike_model::AccountTier::Live)]
    {
        let created = vike_secrets::edit_account_in_unjournalled(
            &settings,
            vike_secrets::AccountEdit::Create { venue, tier, label: None },
        )
        .expect("create the account");
        let id = created.after.expect("a created row").id;
        let value = format!("{venue}-{tier}-SECRET-VALUE");
        vike_secrets::save_owned_credentials_unjournalled(
            &settings,
            &vike_secrets::CredentialOwner::Account(id),
            &[("API_KEY".to_string(), value.clone())],
            vike_secrets::UpsertIntent::Add,
        )
        .expect("write the account's field");
        venue_values.push(value);
    }
    let app = "ctrader-APP-SECRET-VALUE".to_string();
    vike_secrets::save_owned_credentials_unjournalled(
        &settings,
        &vike_secrets::CredentialOwner::Venue("ctrader".to_string()),
        &[("CLIENT_SECRET".to_string(), app.clone())],
        vike_secrets::UpsertIntent::Add,
    )
    .expect("write the venue's application field");
    venue_values.push(app);

    let env = HashMap::from([("VIKE_SETTINGS_DIR".to_string(), settings.display().to_string())]);
    BothKinds { _dir: dir, env, deployment, venue_values }
}

/// **The daemon's REAL boot spec, over a store holding both kinds of row, hands back every
/// non-venue value its consumers read and no venue value** (decision 0146's second reopener, task
/// W15c). The consumers named: the Telegram token and chat/user ids, the alert webhooks. The handle is
/// `NoVenueKeys` whatever `VIKE_NO_LIVE_KEYS` says, the account directory built from it carries key
/// NAMES and no value, and the planted values appear in no `Debug`.
///
/// Mutation: `credentials: Credentials::Read` in `boot_spec` turns the "no venue value" half red;
/// dropping a deployment name from the seed turns the "every value" half red.
#[test]
fn the_tradehub_boot_map_holds_no_venue_value() {
    let BothKinds { _dir, env, deployment, venue_values } = project_holding_both_kinds();
    let cwd = std::env::current_dir().expect("a working directory");
    for word in [None, Some("false"), Some("true")] {
        let mut env = env.clone();
        if let Some(word) = word {
            env.insert("VIKE_NO_LIVE_KEYS".to_string(), word.to_string());
        }
        let booted = vike_boot::boot(&settings::boot_spec(&env, Some(&cwd)))
            .unwrap_or_else(|e| panic!("{word:?}: the daemon's spec boots over the store: {e}"));
        assert_eq!(booted.store.scope(), vike_secrets::CredentialScope::NoVenueKeys, "{word:?}");
        let resolved = booted.credentials.as_ref().expect("the daemon READS the map at boot");
        assert_eq!(resolved.scope, vike_secrets::CredentialScope::NoVenueKeys, "{word:?}");
        let map = resolved.secrets.clone().into_map();

        // Every non-venue value its consumers read is there, byte for byte…
        for (name, value) in &deployment {
            assert_eq!(map.get(name), Some(value), "{word:?}: `{name}` must reach its consumer");
        }
        assert_eq!(
            map.len(),
            deployment.len(),
            "{word:?}: nothing but the deployment rows: {:?}",
            map.keys()
        );
        // …the alerting consumer builds its two targets from it…
        assert_eq!(vike_alerting::webhook_configs_from_env(&map).len(), 2, "{word:?}");
        // …and no venue VALUE is in it, whatever the name it was filed under.
        for secret in &venue_values {
            assert!(!map.values().any(|v| v == secret), "{word:?}: a venue value reached the map");
        }
        assert_eq!(
            resolved.withheld, 3,
            "{word:?}: both accounts' fields and the app field are withheld"
        );

        // The directory this daemon builds from the handle: rows and key NAMES, an empty book.
        let directory = vike_secrets::account_directory::AccountDirectory::read(
            booted.store.accounts(),
            booted.store.account_keys(),
            Ok::<_, String>(vike_secrets::account_credentials::CredentialBook::new()),
        );
        let names = directory.keys().and_then(Result::ok).flatten().expect("the key names read");
        assert_eq!(names.len(), 2, "{word:?}: both accounts' key NAMES stay readable");
        assert_eq!(crate::venue_arming::held_key_names(&directory, "oanda"), 1, "{word:?}");
        assert_eq!(crate::venue_arming::held_key_names(&directory, "binance"), 1, "{word:?}");
        assert_eq!(crate::venue_arming::held_key_names(&directory, "ig"), 0, "{word:?}");
        let shown = format!("{directory:?} {resolved:?}");
        for secret in &venue_values {
            assert!(!shown.contains(secret.as_str()), "{word:?}: a venue value reached a Debug");
        }
    }
}

/// `held_key_names` over a hand-built directory: the account rows of ONE venue, any tier and whether
/// or not active, summed over their key names; an unread or unanswerable directory owns none.
#[test]
fn held_key_names_counts_the_names_of_the_venues_accounts_only() {
    use vike_model::AccountTier::{Demo, Live};
    let label = vike_model::accounts::account_keys::AccountLabel::Default;
    let policy = vike_tradehub_core::AdapterPolicy::default()
        .with_account("oanda", &label, Demo)
        .with_account("oanda", &label, Live)
        .with_account("ig", &label, Demo);
    let rows =
        policy.accounts.rows().and_then(Result::ok).and_then(|a| a.known()).unwrap().to_vec();
    let keys = std::collections::BTreeMap::from([
        (1, vike_secrets::AccountKeys { prefixes: vec![], names: vec!["A".into(), "B".into()] }),
        (2, vike_secrets::AccountKeys { prefixes: vec![], names: vec!["C".into()] }),
        (3, vike_secrets::AccountKeys { prefixes: vec![], names: vec!["D".into()] }),
    ]);
    let directory = vike_secrets::account_directory::AccountDirectory::from_rows(
        vike_secrets::Accounts::Known(rows),
        Some(keys),
    );
    assert_eq!(crate::venue_arming::held_key_names(&directory, "oanda"), 3);
    assert_eq!(crate::venue_arming::held_key_names(&directory, "ig"), 1);
    assert_eq!(crate::venue_arming::held_key_names(&directory, "binance"), 0);
    let unread = vike_secrets::account_directory::AccountDirectory::unread();
    assert_eq!(crate::venue_arming::held_key_names(&unread, "oanda"), 0);
}
