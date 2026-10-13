//! **What the daemon SAYS when a venue's armed accounts dial BOTH networks — and that it says
//! nothing when they do not.**
//!
//! One market feed serves a venue, so a venue with a mainnet account and a testnet account on it
//! cannot have a feed that is right for both. `crates/vike-tradehub/src/venue_arming/tier.rs`'s
//! `feed_tier_from_rows` settles it MAINNET WINS (a mainnet feed under a testnet account costs only
//! testnet fidelity; a testnet feed under a live account prices REAL orders off the testnet book) and
//! does NOT refuse the start (a refusal stops every venue and every mount, fires on the next restart
//! or deploy, and a node that will not start cannot flatten a position —
//! `docs/decisions/0013-degrade-vs-refuse.md`). What it owes the operator in exchange is a loud,
//! structured `warn!`, ONCE PER PROCESS per straddle, naming the venue and the testnet accounts that are
//! being priced off the wrong book — labels and `account` ids, never a credential value. That line is
//! this file's subject, and so is its dedup: the plan is built again whenever a venue is mounted
//! again, and the same straddle said at every build is a line nobody reads twice. The dedup is keyed
//! by the straddle's CONTENT (the venue, the accounts on each network, the account ids that move
//! them), so a DIFFERENT straddle — another testnet account joins — is a new finding and is said.
//! The PLAN each cell gets is `tests/daemon/feed_network_table.rs`'s.
//!
//! # Why a binary of its own
//!
//! The words are asserted through `log_capture::install`, which sets the process's GLOBAL `tracing`
//! default and may be called once — so this file is one test function, silence first and speech
//! second (the shape `unaddressable_account_silence.rs` documents), and it cannot be a `daemon` group
//! member: that group's members share a process with tests that install their own subscribers.
//!
//! The collector records an event's `message` only, so the message carries everything an operator
//! needs; the structured fields beside it (the venue, the accounts on each network and the testnet
//! accounts' ids) are for a log pipeline.

mod log_capture;

use std::collections::HashMap;

use vike_hyperliquid::base::config::Network;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_mount::{MakerMountConfig, MountPolicy};
use vike_tradehub::VenuePlan;
use vike_tradehub::feeds::venue_feed_plan;

/// The value every fixture credential carries — asserted ABSENT from what the daemon says.
const FAKE_SECRET: &str = "fixture-credential-value-that-must-never-be-echoed";

/// The phrase only the straddle warning contains — what every count below keys on.
const STRADDLE: &str = "ONE market feed per venue";

/// ALT's and BETA's `account.id`s: distinctive, so a warning naming them cannot match by accident.
const ALT_ID: i64 = 4242;
const BETA_ID: i64 = 4343;

/// What an account holds: its private key (the whole live gate), or nothing.
const NONE: &[&str] = &[];
const KEY: &[&str] = &["PRIVATE_KEY"];
/// …and the same key plus the optional master address — more fields, the same straddle by content.
const KEY_AND_ADDRESS: &[&str] = &["PRIVATE_KEY", "ACCOUNT_ADDRESS"];

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

fn beta() -> AccountLabel {
    AccountLabel::parse("BETA").expect("a legal label")
}

/// The fields an account holds, each carrying the fixture secret.
fn held(fields: &[&str]) -> Vec<(String, String)> {
    fields.iter().map(|f| ((*f).to_string(), FAKE_SECRET.to_string())).collect()
}

/// Plant `fields` on the `label` account of the policy (a no-op for an empty set). The tier is the
/// ROW's — no credential carries one (decision 0156 item 6).
fn holding(
    policy: MountPolicy,
    label: &AccountLabel,
    tier: AccountTier,
    fields: &[&str],
) -> MountPolicy {
    if fields.is_empty() {
        return policy;
    }
    let owned = held(fields);
    let pairs: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    policy.with_credentials("hyperliquid", label, tier, &pairs)
}

/// One ACTIVE hyperliquid `account` row for `label` at `tier`, with the caller's id.
fn row(id: i64, label: &AccountLabel, tier: AccountTier) -> vike_secrets::Account {
    vike_secrets::Account {
        id,
        venue: "hyperliquid".to_string(),
        tier,
        label: label.text().map(str::to_string),
        venue_account_id: None,
        parent_id: None,
        active: true,
        last_verified_at: None,
        max_exposure: None,
    }
}

/// The default account's active `live` row holding `default`, and ALT's own row (holding `alt_fields`)
/// when it has one.
fn accounts(default: &[&str], alt_fields: &[&str], alt_tier: Option<AccountTier>) -> MountPolicy {
    let policy =
        MountPolicy::default().with_account_row(row(1, &AccountLabel::Default, AccountTier::Live));
    let policy = holding(policy, &AccountLabel::Default, AccountTier::Live, default);
    match alt_tier {
        Some(tier) => {
            let policy = policy.with_account_row(row(ALT_ID, &alt(), tier));
            holding(policy, &alt(), tier, alt_fields)
        }
        None => policy,
    }
}

/// [`accounts`] plus BETA's own active `demo` row, holding a key.
fn accounts_with_beta(
    default: &[&str],
    alt_fields: &[&str],
    alt_tier: Option<AccountTier>,
) -> MountPolicy {
    let policy = accounts(default, alt_fields, alt_tier).with_account_row(row(
        BETA_ID,
        &beta(),
        AccountTier::Demo,
    ));
    holding(policy, &beta(), AccountTier::Demo, KEY)
}

fn plan(policy: &MountPolicy) -> Network {
    let cfg = MakerMountConfig::crypto("hyperliquid", "BTC", 0.5, 0.001);
    match venue_feed_plan(&cfg, &HashMap::new(), policy).expect("a straddle is not a refusal") {
        VenuePlan::Hyperliquid(network) => network,
        other => panic!("hyperliquid planned {other:?}"),
    }
}

fn straddle_lines(log: &log_capture::Log) -> Vec<String> {
    log_capture::lines(log).into_iter().filter(|line| line.contains(STRADDLE)).collect()
}

#[test]
fn a_straddle_is_said_once_per_process_and_nothing_else_is() {
    let log = log_capture::install();

    // ── SILENCE. Every shape in which the armed accounts do NOT straddle.
    let demo = Some(AccountTier::Demo);
    let live = Some(AccountTier::Live);
    // (A `demo` row holding "only live keys" and a `live` row holding "only demo keys" were cells
    // while a credential carried a tier; a field carries none, so the row's tier is the whole story.)
    for (label, default, alt_fields, alt_tier, want) in [
        ("one account, live", KEY, NONE, None, Network::Mainnet),
        ("ALT's `demo` row holding no keys", KEY, NONE, demo, Network::Mainnet),
        ("ALT's `live` row armed live beside a live default", KEY, KEY, live, Network::Mainnet),
        ("only ALT armed, on testnet", NONE, KEY, demo, Network::Testnet),
        // No-downgrade (decision 0119): a `live` ALT holding nothing is PAPER, never a testnet
        // account — so there is nothing on testnet to straddle with.
        ("ALT's `live` row holding nothing", KEY, NONE, live, Network::Mainnet),
    ] {
        let got = plan(&accounts(default, alt_fields, alt_tier));
        assert_eq!(got, want, "{label}");
        assert!(
            straddle_lines(&log).is_empty(),
            "{label}: no straddle, so nothing may be said: {:?}",
            straddle_lines(&log)
        );
    }

    // ── SPEECH, ONCE. A live default account beside ALT's `demo` row armed on its demo keys. The
    // FIRST plan build that sees this straddle says it; every later build of the SAME straddle (a
    // second mount of the venue, a re-plan) says nothing more — one line per straddle per process,
    // not one per plan build.
    let first = {
        let before = straddle_lines(&log).len();
        let got = plan(&accounts(KEY, KEY, demo));
        assert_eq!(got, Network::Mainnet, "mainnet wins, and the plan is not refused");
        let said = straddle_lines(&log);
        assert_eq!(said.len(), before + 1, "the first build says it, once: {said:?}");
        said.last().expect("one line").clone()
    };
    assert!(first.starts_with("[WARN] "), "loud means warn!, not info!: {first}");
    let alt_id = ALT_ID.to_string();
    for needle in ["hyperliquid", "ALT", "MAINNET", "TESTNET", "set-tier", alt_id.as_str()] {
        assert!(first.contains(needle), "the warning must name {needle}: {first}");
    }
    assert!(!first.contains("policy.accounts"), "the deleted settings key must be gone: {first}");
    assert!(!first.contains(FAKE_SECRET), "no credential value, ever: {first}");

    // THE SAME STRADDLE AGAIN. A second plan build over the identical accounts — and one over a
    // store whose default account merely holds more keys, which is the same straddle by content —
    // adds NOTHING, and still plans mainnet.
    for default in [KEY, KEY_AND_ADDRESS, KEY] {
        let before = straddle_lines(&log).len();
        let got = plan(&accounts(default, KEY, demo));
        assert_eq!(got, Network::Mainnet, "the plan is the same every time");
        assert_eq!(
            straddle_lines(&log).len(),
            before,
            "a straddle already said in this process is not said again: {:?}",
            straddle_lines(&log)
        );
    }

    // A DIFFERENT STRADDLE is a different finding and is said: a second testnet account (BETA)
    // changes WHICH accounts are priced off the wrong book, so the operator's picture changes. And
    // that one, in turn, is said once.
    for round in 0..2 {
        let before = straddle_lines(&log).len();
        let got = plan(&accounts_with_beta(KEY, KEY, demo));
        assert_eq!(got, Network::Mainnet);
        let said = straddle_lines(&log);
        let expected = before + usize::from(round == 0);
        assert_eq!(
            said.len(),
            expected,
            "round {round}: a new straddle says itself once, then is silent: {said:?}"
        );
        if round == 0 {
            let line = said.last().expect("one line");
            assert!(line.contains("ALT") && line.contains("BETA"), "names both accounts: {line}");
            assert!(line.contains(&BETA_ID.to_string()), "names BETA's id too: {line}");
        }
    }
}
