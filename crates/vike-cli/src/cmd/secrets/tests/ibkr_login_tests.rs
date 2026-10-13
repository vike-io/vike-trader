//! Unit tests for `secrets ibc-start` / `ibkr-cp-login`: the grammar, the session pair check both
//! verbs run before a value is read (decision 0140), and the per-tier names the scripts spell. The
//! shipped binary, the store and the child processes are driven by
//! `crates/vike-cli/tests/secrets_cli/ibkr_login.rs`.
//!
//! ⚠ No credential NAME is spelled as a literal here: they come from `LOGIN_NAMES`, because an
//! env-shaped literal in this tree is read by the settings registry's sweep as a read.

use vike_model::{AccountTier, IbkrBackend, IbkrSession};
use vike_secrets::VenueSettingRow;
use vike_secrets::venue_setting::VenueSettings;

use super::ibkr_login::{
    IbkrRow, LIVE_INI_FILE, LIVE_LOGIN_NAMES, LIVE_SETTINGS_DIR, LIVE_TRADING_MODE, LOGIN_NAMES,
    Launcher, Login, StoredSession, TRADING_MODE, gateway_tier, session_verdict,
};
use super::*;

fn parse_of(args: &[&str]) -> Result<Args, args::Stop> {
    parse(args.iter().map(|s| (*s).to_string()))
}

const OPERANDS: [&str; 6] =
    ["--root", "/p/bin/ibkr-gateway", "--gateway-version", "1045", "--java-path", "/p/jre/bin"];

#[test]
fn ibc_start_needs_all_three_operands_and_names_the_verb_in_the_refusal() {
    for skip in 0..3 {
        let mut args = vec!["ibc-start"];
        for (i, pair) in OPERANDS.chunks(2).enumerate() {
            if i != skip {
                args.extend(pair);
            }
        }
        let err = parse_of(&args).unwrap_err();
        assert!(err.contains("`ibc-start` needs --root DIR"), "{args:?}: {err}");
    }
    assert!(parse_of(&["ibc-start"]).is_err());
    let mut full = vec!["ibc-start"];
    full.extend(OPERANDS);
    let ok = parse_of(&full).unwrap();
    assert_eq!(ok.sub, Sub::IbcStart);
    assert_eq!(ok.root.as_deref(), Some("/p/bin/ibkr-gateway"));
    assert_eq!(ok.gateway_version.as_deref(), Some("1045"));
    assert_eq!(ok.java_path.as_deref(), Some("/p/jre/bin"));
    assert!(!ok.check);
}

#[test]
fn the_operands_are_refused_off_ibc_start_without_quoting_their_value() {
    for sub in ["list", "path", "set", "ibkr-cp-login", "init"] {
        let mut args = vec![sub];
        if sub == "set" {
            args.push("SOME_KEY");
        }
        args.extend(["--root", "hunter2-looks-like-a-password"]);
        let err = parse_of(&args).unwrap_err();
        assert!(err.contains("apply to `ibc-start` only"), "{sub}: {err}");
        assert!(!err.contains("hunter2"), "{sub}: the refusal echoed the value: {err}");
    }
}

#[test]
fn tier_picks_the_account_on_both_launchers_only_and_defaults_to_demo() {
    for verb in ["ibc-start", "ibkr-cp-login"] {
        let tier_of = |extra: &[&str]| {
            let mut args = vec![verb];
            args.extend(extra);
            if verb == "ibc-start" {
                args.extend(OPERANDS);
            }
            parse_of(&args).map(|a| a.tier)
        };
        assert_eq!(tier_of(&[]).unwrap(), None, "{verb}: absent = demo, as before the flag");
        assert_eq!(tier_of(&["--tier", "demo"]).unwrap().as_deref(), Some("demo"));
        assert_eq!(tier_of(&["--tier", "live"]).unwrap().as_deref(), Some("live"));
        // Case does not matter: `--tier DEMO` / `--tier LIVE` name the same accounts.
        assert_eq!(tier_of(&["--tier", "LIVE"]).unwrap().as_deref(), Some("LIVE"));
        // `paper` is an `account` tier, not a gateway; anything else is no gateway either.
        for bad in ["paper", "sim", "livee"] {
            let err = tier_of(&["--tier", bad]).unwrap_err();
            assert!(err.contains(verb) && err.contains("demo") && err.contains("live"), "{err}");
        }
    }
    for sub in ["list", "path", "init", "accounts"] {
        let err = parse_of(&[sub, "--tier", "live"]).unwrap_err();
        assert!(err.contains("--tier applies to") && err.contains("ibkr-cp-login"), "{sub}: {err}");
    }
}

/// `--check` is the session probe: on the two launchers only, with no install operands beside it
/// (it starts nothing), and with `--tier` (the pair is per tier).
#[test]
fn check_is_a_launcher_flag_that_needs_and_takes_no_operand() {
    let ok = parse_of(&["ibc-start", "--check"]).unwrap();
    assert!(ok.check && ok.root.is_none(), "--check needs no install operands");
    let ok = parse_of(&["ibc-start", "--check", "--tier", "live"]).unwrap();
    assert!(ok.check && ok.tier.as_deref() == Some("live"));
    assert!(parse_of(&["ibkr-cp-login", "--check"]).unwrap().check);
    assert!(parse_of(&["ibkr-cp-login", "--tier", "live", "--check"]).unwrap().check);

    let mut with_operands = vec!["ibc-start", "--check"];
    with_operands.extend(OPERANDS);
    let err = parse_of(&with_operands).unwrap_err();
    assert!(err.contains("--check") && err.contains("starts nothing"), "{err}");
    assert!(parse_of(&["ibc-start", "--check=yes"]).is_err(), "--check takes no value");
    for sub in ["list", "path", "init", "accounts"] {
        let err = parse_of(&[sub, "--check"]).unwrap_err();
        assert!(err.contains("--check applies to"), "{sub}: {err}");
    }
}

#[test]
fn the_gateway_tier_words_and_the_per_tier_names_are_what_the_scripts_spell() {
    assert_eq!(gateway_tier("live"), Some(AccountTier::Live));
    assert_eq!(gateway_tier("demo"), Some(AccountTier::Demo));
    // Case-insensitive and trimmed, as `ibc-start --tier DEMO` always was.
    assert_eq!(gateway_tier("DEMO"), Some(AccountTier::Demo));
    assert_eq!(gateway_tier(" Live "), Some(AccountTier::Live));
    // `paper` IS a tier (the account verb's), but no gateway logs in for it: refused at the parse.
    for not_a_gateway in ["paper", "PAPER", "sim"] {
        assert_eq!(gateway_tier(not_a_gateway), None, "{not_a_gateway:?}");
    }

    let (user, pass) = LIVE_LOGIN_NAMES;
    assert!(user.contains("LIVE") && pass.contains("LIVE"), "{user} {pass}");
    assert!(user.ends_with("USERNAME") && pass.ends_with("PASSWORD"), "{user} {pass}");
    assert_ne!((user, pass), LOGIN_NAMES, "the two tiers read different pairs");
    let (user, pass) = LOGIN_NAMES;
    assert!(user.contains("DEMO") && pass.contains("DEMO"), "{user} {pass}");
    assert_eq!(TRADING_MODE, "paper", "the DEMO tier is IBC's paper mode");
    assert_eq!(LIVE_TRADING_MODE, "live", "the LIVE tier is IBC's live mode");
    // The live gateway shares NOTHING with the demo one: its own ini and its own settings directory
    // (the demo's settings directory is the install directory `jts/`, unchanged).
    assert_ne!(LIVE_INI_FILE, "config.ini");
    assert!(LIVE_SETTINGS_DIR != "jts" && LIVE_SETTINGS_DIR.starts_with("jts"));
    // …and the scripts spell the very same two names. This test does not read them (a crate's tests
    // that read `deploy/` need a reader row in `xtask`'s tables); the OTHER half is
    // `crates/vike-ops/tests/container_deploy/deploy_shadowed_store_gate.rs`'s
    // `the_live_instance_table_is_what_the_verb_and_the_runbook_spell`, which pins the same two
    // literals in `instance.sh` — so a rename on either side reddens one of the two.
    assert_eq!((LIVE_INI_FILE, LIVE_SETTINGS_DIR), ("config-live.ini", "jts-live"));
}

#[test]
fn the_cp_login_takes_no_flag_a_script_could_smuggle_a_value_through() {
    assert_eq!(parse_of(&["ibkr-cp-login"]).unwrap().sub, Sub::IbkrCpLogin);
    for flag in ["--root", "--from-env", "--key", "--dry-run", "--out", "--base"] {
        assert!(parse_of(&["ibkr-cp-login", flag, "x"]).is_err(), "{flag} must be refused");
    }
    assert!(parse_of(&["ibkr-cp-login", "stray"]).is_err(), "a positional must be refused");
}

#[test]
fn a_login_cannot_be_printed_by_accident() {
    let l = Login::for_tests("the-user", "the-password");
    let shown = format!("{l:?}");
    assert!(!shown.contains("the-user") && !shown.contains("the-password"), "{shown}");
}

// --- the session pair check (decision 0140) ---

fn row(id: i64, tier: AccountTier, label: Option<&str>, active: bool) -> IbkrRow {
    IbkrRow { id, tier, label: label.map(str::to_string), active }
}

/// The ibkr `venue_setting` rows `(tier, field, value)`; `None` tier = machine-scoped.
fn ibkr_settings(rows: &[(Option<AccountTier>, &str, &str)]) -> VenueSettings {
    let rows: Vec<VenueSettingRow> = rows
        .iter()
        .map(|(tier, field, value)| VenueSettingRow {
            venue: "ibkr".to_string(),
            tier: *tier,
            field: field.to_ascii_uppercase(),
            value: (*value).to_string(),
        })
        .collect();
    VenueSettings::from_rows("ibkr", &rows)
}

fn stored(rows: Vec<IbkrRow>, backend: &[(Option<AccountTier>, &str, &str)]) -> StoredSession {
    StoredSession::resolve(rows, Some(&ibkr_settings(backend)), None)
}

/// The backend word is read exactly as the bridge's config loader reads it: the resolved tier's own
/// row, never a machine-scoped one, trimmed, a blank row reading as no row (= socket). Only ACTIVE
/// rows state a tier, and paper rows never reach the verdict.
#[test]
fn the_stored_session_reads_the_rows_the_mount_reads() {
    use AccountTier::{Demo, Live};
    let demo = || vec![row(3, Demo, None, true)];
    let pair = |s: &StoredSession| s.answer.clone().unwrap();
    let socket_demo = IbkrSession::Pair { backend: IbkrBackend::Socket, tier: Demo };
    let cpapi_demo = IbkrSession::Pair { backend: IbkrBackend::Cpapi, tier: Demo };
    assert_eq!(pair(&stored(demo(), &[])), socket_demo, "no backend row = socket");
    assert_eq!(pair(&stored(demo(), &[(Some(Demo), "backend", " cpapi ")])), cpapi_demo);
    assert_eq!(pair(&stored(demo(), &[(Some(Demo), "backend", "  ")])), socket_demo, "blank");
    assert_eq!(
        pair(&stored(demo(), &[(None, "backend", "cpapi")])),
        socket_demo,
        "a machine-scoped backend row is not the tier's row"
    );
    assert_eq!(
        pair(&stored(demo(), &[(Some(Live), "backend", "cpapi")])),
        socket_demo,
        "the OTHER tier's backend row does not speak for this tier"
    );
    // A labelled row states the tier as well as an unlabelled one (the venue-wide scope).
    assert_eq!(pair(&stored(vec![row(9, Demo, Some("HEDGE"), true)], &[])), socket_demo);
    // Inactive rows state nothing; demo AND live active, of any labels, is a conflict.
    assert_eq!(pair(&stored(vec![row(3, Demo, None, false)], &[])), IbkrSession::Off);
    let both = vec![row(3, Demo, None, true), row(5, Live, Some("HEDGE"), true)];
    assert_eq!(pair(&stored(both, &[])), IbkrSession::Conflict);
    let bad = stored(demo(), &[(Some(Demo), "backend", "grpc")]);
    assert!(bad.answer.is_err(), "a word that is no backend is never a default: {bad:?}");
}

/// ⚠ THE RULE OF EACH VERB, over every shape: a launcher admits EXACTLY its own `(product, tier)` and
/// refuses every other answer, each refusal naming the pair the store holds and the rows that set it.
#[test]
fn each_launcher_admits_only_its_own_pair() {
    use AccountTier::{Demo, Live, Paper};
    let shapes: Vec<(&str, StoredSession)> = vec![
        ("socket-demo", stored(vec![row(3, Demo, None, true)], &[])),
        ("socket-live", stored(vec![row(4, Live, None, true)], &[])),
        ("cpapi-demo", stored(vec![row(3, Demo, None, true)], &[(Some(Demo), "backend", "cpapi")])),
        ("cpapi-live", stored(vec![row(4, Live, None, true)], &[(Some(Live), "backend", "cpapi")])),
        ("oauth-demo", stored(vec![row(3, Demo, None, true)], &[(Some(Demo), "backend", "oauth")])),
        ("off", stored(vec![], &[])),
        ("conflict", stored(vec![row(3, Demo, None, true), row(8, Live, Some("X"), true)], &[])),
        ("unparseable", stored(vec![row(3, Demo, None, true)], &[(Some(Demo), "backend", "grpc")])),
    ];
    let admitted = |l: Launcher, t: AccountTier| match (l, t) {
        (Launcher::IbcStart, Demo) => "socket-demo",
        (Launcher::IbcStart, Live) => "socket-live",
        (Launcher::CpLogin, Demo) => "cpapi-demo",
        (Launcher::CpLogin, Live) => "cpapi-live",
        (_, Paper) => unreachable!("`gateway_tier` refuses `paper`: no launcher holds it"),
    };
    for launcher in [Launcher::IbcStart, Launcher::CpLogin] {
        for tier in [Demo, Live] {
            for (name, s) in &shapes {
                let verdict = session_verdict(launcher, tier, s);
                if *name == admitted(launcher, tier) {
                    assert!(
                        verdict.is_ok(),
                        "{launcher:?} {tier:?} must admit {name}: {verdict:?}"
                    );
                    continue;
                }
                let why = verdict.expect_err(name);
                let want = format!("({}, {})", launcher.backend(), tier);
                assert!(why.contains(&want), "{launcher:?} {tier:?} {name}: names its pair: {why}");
                assert!(why.contains("Nothing was read or run"), "{name}: {why}");
                assert!(why.contains("ibkr session in the settings database"), "{name}: {why}");
                for r in &s.rows {
                    assert!(why.contains(&format!("account row {}", r.id)), "{name}: {why}");
                }
            }
        }
    }
}

/// What each refusal says: the pair the store HOLDS and the verb that logs in for it, the rows of a
/// conflict by id, the repair for "off", the backend row that cannot be read.
#[test]
fn each_refusal_names_the_held_pair_and_the_repair() {
    use AccountTier::{Demo, Live};
    let cp_demo = stored(vec![row(3, Demo, None, true)], &[(Some(Demo), "backend", "cpapi")]);
    let why = session_verdict(Launcher::IbcStart, AccountTier::Demo, &cp_demo).unwrap_err();
    assert!(why.contains("holds (cpapi, demo)"), "{why}");
    assert!(why.contains("vike-cli secrets ibkr-cp-login`"), "the held pair's verb: {why}");
    assert!(why.contains("venue.ibkr.demo.backend = cpapi"), "the row that set it: {why}");

    let socket_live = stored(vec![row(4, Live, None, true)], &[]);
    let why = session_verdict(Launcher::CpLogin, AccountTier::Demo, &socket_live).unwrap_err();
    assert!(why.contains("holds (socket, live)"), "{why}");
    assert!(why.contains("ibc-start --tier live") && why.contains("phone"), "{why}");
    assert!(why.contains("venue.ibkr.live.backend: not set (socket, the default)"), "{why}");

    let conflict = stored(vec![row(3, Demo, None, true), row(8, Live, Some("HEDGE"), true)], &[]);
    let why = session_verdict(Launcher::IbcStart, AccountTier::Demo, &conflict).unwrap_err();
    assert!(why.contains("CONFLICT") && why.contains("demo: 3; live: 8"), "{why}");
    assert!(why.contains("account deactivate --id N") && why.contains("label HEDGE"), "{why}");

    let (user, pass) = LOGIN_NAMES;
    let empty = stored(vec![], &[]);
    let why = session_verdict(Launcher::IbcStart, AccountTier::Demo, &empty).unwrap_err();
    assert!(why.contains("NO IBKR session") && why.contains("no ibkr account row"), "{why}");
    assert!(why.contains(&format!("vike-cli secrets set {user}")) && why.contains(pass), "{why}");
    let (live_user, _) = LIVE_LOGIN_NAMES;
    let why = session_verdict(Launcher::CpLogin, AccountTier::Live, &empty).unwrap_err();
    assert!(why.contains(live_user) && why.contains("--tier live --no-label"), "{why}");

    let dead = stored(vec![row(6, Demo, None, false)], &[]);
    let why = session_verdict(Launcher::IbcStart, AccountTier::Demo, &dead).unwrap_err();
    assert!(why.contains("row(s) 6 are DEACTIVATED") && why.contains("activate --id N"), "{why}");
    assert!(why.contains("account row 6: ibkr demo, unlabelled, deactivated"), "{why}");

    let no_store = StoredSession::resolve(vec![], None, Some("no db here".to_string()));
    let why = session_verdict(Launcher::IbcStart, AccountTier::Demo, &no_store).unwrap_err();
    assert!(why.contains("no db here"), "the absent store is said: {why}");

    let bad = stored(vec![row(3, Demo, None, true)], &[(Some(Demo), "backend", "grpc")]);
    let why = session_verdict(Launcher::CpLogin, AccountTier::Demo, &bad).unwrap_err();
    assert!(why.contains("`grpc`") && why.contains("config set venue.ibkr.demo.backend"), "{why}");
    assert!(why.contains("venue.ibkr.demo.backend = grpc"), "{why}");
}
