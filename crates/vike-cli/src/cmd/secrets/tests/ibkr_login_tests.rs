//! Unit tests for `secrets ibc-start` / `ibkr-cp-login`: the grammar, the single account both verbs
//! log in to, and the one source-level mirror of the ibkr mount's arming rule. The shipped binary,
//! the store and the child processes are driven by `crates/vike-cli/tests/secrets_cli/ibkr_login.rs`.
//!
//! ⚠ No credential NAME is spelled as a literal here: they come from `LOGIN_NAMES`, because an
//! env-shaped literal in this tree is read by the settings registry's sweep as a read.

use super::ibkr_login::{
    GatewayTier, LIVE_INI_FILE, LIVE_LOGIN_NAMES, LIVE_SETTINGS_DIR, LIVE_TRADING_MODE,
    LOGIN_NAMES, Login, TRADING_MODE, account_rows_verdict,
};
use super::*;

fn parse_of(args: &[&str]) -> Result<Args, String> {
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
fn tier_picks_the_gateway_on_ibc_start_only_and_defaults_to_demo() {
    let tier_of = |extra: &[&str]| {
        let mut args = vec!["ibc-start"];
        args.extend(extra);
        args.extend(OPERANDS);
        parse_of(&args).map(|a| a.tier)
    };
    assert_eq!(tier_of(&[]).unwrap(), None, "absent = the demo gateway, as before the flag");
    assert_eq!(tier_of(&["--tier", "demo"]).unwrap().as_deref(), Some("demo"));
    assert_eq!(tier_of(&["--tier", "live"]).unwrap().as_deref(), Some("live"));
    assert_eq!(tier_of(&["--tier=LIVE"]).unwrap().as_deref(), Some("LIVE"));
    // `paper` is an `account` tier, not a gateway; anything else is no gateway either.
    for bad in ["paper", "sim", "livee"] {
        let err = tier_of(&["--tier", bad]).unwrap_err();
        assert!(err.contains("demo") && err.contains("live"), "{bad:?}: {err}");
    }
    // The Client Portal driver has no live tier, and every other verb still refuses the flag.
    assert!(parse_of(&["ibkr-cp-login", "--tier", "live"]).unwrap_err().contains("--tier"));
    for sub in ["list", "path", "init", "accounts"] {
        let err = parse_of(&[sub, "--tier", "live"]).unwrap_err();
        assert!(err.contains("--tier applies to") && err.contains("ibc-start"), "{sub}: {err}");
    }
}

#[test]
fn the_gateway_tier_words_and_the_per_tier_names_are_what_the_scripts_spell() {
    assert_eq!(GatewayTier::parse(" Live "), Some(GatewayTier::Live));
    assert_eq!(GatewayTier::parse("DEMO"), Some(GatewayTier::Demo));
    assert_eq!(GatewayTier::parse("paper"), None);

    let (user, pass) = LIVE_LOGIN_NAMES;
    assert!(user.contains("LIVE") && pass.contains("LIVE"), "{user} {pass}");
    assert!(user.ends_with("USERNAME") && pass.ends_with("PASSWORD"), "{user} {pass}");
    assert_ne!((user, pass), LOGIN_NAMES, "the two tiers read different pairs");
    assert_eq!(LIVE_TRADING_MODE, "live");
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

/// The off switch's verdicts, pure: the cases the shipped binary cannot reach (a LIVE start with NO
/// live row cannot be built through `set`, which creates the row with the pair).
#[test]
fn the_account_row_verdict_is_demo_lenient_and_live_strict() {
    use GatewayTier::{Demo, Live};
    // DEMO: no row is not a refusal (the credentials are the gate), a dead row is, a live one is not.
    assert!(account_rows_verdict(&[], Demo).is_ok());
    assert!(account_rows_verdict(&[(4, true)], Demo).is_ok());
    assert!(account_rows_verdict(&[(4, false), (9, true)], Demo).is_ok(), "ANY active row admits");
    let dead = account_rows_verdict(&[(4, false)], Demo).unwrap_err();
    assert!(dead.contains("DEMO") && dead.contains("DEACTIVATED"), "{dead}");
    assert!(dead.contains("account activate --id 4") && dead.contains("Nothing was run"), "{dead}");

    // LIVE: an active row admits; a dead row refuses; NO row refuses, naming the repair.
    assert!(account_rows_verdict(&[(5, true)], Live).is_ok());
    assert!(account_rows_verdict(&[(5, false), (6, true)], Live).is_ok());
    let dead = account_rows_verdict(&[(5, false)], Live).unwrap_err();
    assert!(dead.contains("LIVE") && dead.contains("DEACTIVATED"), "{dead}");
    assert!(dead.contains("account activate --id 5"), "{dead}");
    let none = account_rows_verdict(&[], Live).unwrap_err();
    assert!(none.contains("no ibkr LIVE account row"), "{none}");
    assert!(none.contains("account add --venue ibkr --tier live"), "{none}");
    assert!(none.contains("Nothing was run"), "{none}");
}

#[test]
fn the_cp_login_takes_no_flag_a_script_could_smuggle_a_value_through() {
    assert_eq!(parse_of(&["ibkr-cp-login"]).unwrap().sub, Sub::IbkrCpLogin);
    for flag in ["--root", "--from-env", "--key", "--dry-run", "--out", "--base"] {
        assert!(parse_of(&["ibkr-cp-login", flag, "x"]).is_err(), "{flag} must be refused");
    }
    assert!(parse_of(&["ibkr-cp-login", "stray"]).is_err(), "a positional must be refused");
}

/// ⚠ The plain gateway this verb starts is the PAPER one, `--tier live` the REAL-MONEY one, and the
/// rule for which account each logs in to is the ibkr MOUNT's, which cannot be called from here
/// (`vike-ibkr` is a layer-40 venue crate), so it is mirrored — and this holds the mirror to the
/// mount's source: the DEMO tier still arms whenever its config loads, the LIVE tier is reachable
/// only through a `live` account tier (`MountInputs::live_permitted`) AND an ACTIVE live account
/// row (the three `LiveRow` verdicts the live start mirrors), and the mount's book-identity row
/// spells the same tier words the pairs' names carry. When the mount's rule moves, `LOGIN_NAMES`,
/// `LIVE_LOGIN_NAMES`, `account_rows_verdict` and this test change together.
#[test]
fn the_account_is_the_one_the_ibkr_mount_arms() {
    let (user, pass) = LOGIN_NAMES;
    assert!(user.contains("DEMO") && pass.contains("DEMO"));
    assert!(user.ends_with("USERNAME") && pass.ends_with("PASSWORD"));
    assert_eq!(TRADING_MODE, "paper", "the DEMO tier is IBC's paper mode");
    assert_eq!(LIVE_TRADING_MODE, "live", "the LIVE tier is IBC's live mode");

    let mount = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("bridges")
        .join("vike-ibkr")
        .join("src")
        .join("mount.rs");
    let Ok(text) = std::fs::read_to_string(&mount) else {
        eprintln!(
            "SKIPPED: {} is not beside this crate (a source tree without the bridges)",
            mount.display()
        );
        return;
    };
    // Comments dropped and every run of whitespace collapsed, so a reformat cannot hide the rule.
    let code: String = text
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        code.contains("Resolution::Armed { tier: Tier::Demo,"),
        "the ibkr mount no longer arms the DEMO tier in `resolve` — re-derive which account the \
         gateway must log in to, and this module's LOGIN_NAMES/TRADING_MODE with it"
    );
    // The mount arms the LIVE tier now, but ONLY behind a `live` account tier: a live arm that did
    // not read `live_permitted` would arm real money on a box whose gateway this verb logged in to
    // the paper account. The tier is read before anything else (`IbkrVenueMount::live_tier`).
    assert!(
        code.contains("tier: Tier::Live") && code.contains("!inputs.live_permitted"),
        "the ibkr mount's LIVE arm is no longer behind the account's `live` tier — the gateway's \
         one-account rule no longer mirrors it"
    );
    // The live start's account-row rule (`account_rows_verdict`): an ACTIVE row admits, a deactivated
    // row refuses, NO row refuses, and a store that cannot say refuses — the mount's `LiveRow`.
    assert!(
        code.contains("LiveRow::Active")
            && code.contains("LiveRow::Deactivated")
            && code.contains("LiveRow::NoRow")
            && code.contains("LiveRow::Unknowable"),
        "the mount's LIVE account-row verdicts changed — `account_rows_verdict` and \
         `refuse_an_unarmed_account` mirror them"
    );
    assert!(
        code.contains("PaperCause::LiveTierNotWired"),
        "the mount no longer says a LIVE-only store is not wired — the refusal in `read_login` \
         that the OTHER tier's pair is not used rests on it"
    );
    assert!(
        code.contains("demo_tiers: &[\"DEMO\"]") && code.contains("live_tiers: &[\"LIVE\"]"),
        "the mount's book-identity row changed its tier words"
    );
}

#[test]
fn a_login_cannot_be_printed_by_accident() {
    let l = Login::for_tests("the-user", "the-password");
    let shown = format!("{l:?}");
    assert!(!shown.contains("the-user") && !shown.contains("the-password"), "{shown}");
}
