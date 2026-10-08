//! Unit tests for `secrets ibc-start` / `ibkr-cp-login`: the grammar, the single account both verbs
//! log in to, and the one source-level mirror of the ibkr mount's arming rule. The shipped binary,
//! the store and the child processes are driven by `crates/vike-cli/tests/secrets_cli/ibkr_login.rs`.
//!
//! ⚠ No credential NAME is spelled as a literal here: they come from `LOGIN_NAMES`, because an
//! env-shaped literal in this tree is read by the settings registry's sweep as a read.

use super::ibkr_login::{LOGIN_NAMES, Login, TRADING_MODE};
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
    for sub in ["list", "path", "set", "ibkr-cp-login", "migrate"] {
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
fn there_is_no_tier_flag_on_either_gateway_verb() {
    // The account is decided by the ibkr mount's rule, not by an operator: `--tier` stays an
    // `account` flag only.
    let mut full = vec!["ibc-start", "--tier", "live"];
    full.extend(OPERANDS);
    assert!(parse_of(&full).unwrap_err().contains("--tier applies to"));
    assert!(parse_of(&["ibkr-cp-login", "--tier", "live"]).unwrap_err().contains("--tier"));
}

#[test]
fn the_cp_login_takes_no_flag_a_script_could_smuggle_a_value_through() {
    assert_eq!(parse_of(&["ibkr-cp-login"]).unwrap().sub, Sub::IbkrCpLogin);
    for flag in ["--root", "--from-env", "--key", "--dry-run", "--out", "--base"] {
        assert!(parse_of(&["ibkr-cp-login", flag, "x"]).is_err(), "{flag} must be refused");
    }
    assert!(parse_of(&["ibkr-cp-login", "stray"]).is_err(), "a positional must be refused");
}

/// ⚠ The gateway logs in to the account the ibkr MOUNT arms, and that rule cannot be called from
/// here (`vike-ibkr` is a layer-40 venue crate), so it is mirrored — and this holds the mirror to
/// the mount's source: DEMO is the one armed tier, a LIVE-only store is named and not mounted, and
/// the mount's book-identity row spells the same tier words the pair's names carry.
#[test]
fn the_account_is_the_one_the_ibkr_mount_arms() {
    let (user, pass) = LOGIN_NAMES;
    assert!(user.contains("DEMO") && pass.contains("DEMO"));
    assert!(user.ends_with("USERNAME") && pass.ends_with("PASSWORD"));
    assert_eq!(TRADING_MODE, "paper", "the DEMO tier is IBC's paper mode");

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
    assert!(
        !code.contains("tier: Tier::Live"),
        "the ibkr mount now names a LIVE arm: the gateway's one-account rule no longer mirrors it"
    );
    assert!(
        code.contains("PaperCause::LiveTierNotWired"),
        "the mount no longer says a LIVE-only store is not wired — the refusal in `read_login` \
         that a LIVE pair is not used rests on it"
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
