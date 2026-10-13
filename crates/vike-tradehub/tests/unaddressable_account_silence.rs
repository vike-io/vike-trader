//! **What a mount says about a SECOND account — and, on a box that has none, what it must not say.**
//!
//! ⚠ **This file's original assertion could no longer fail, and it has been replaced rather than
//! left standing.** It drove `make_engine_accounts` on a single-account box and asserted the
//! process-wide unaddressable-account report stayed silent. That report is keyed on
//! `vike_config::ArmingBlock::NoAccountSupport`, and since dukascopy joined
//! `vike_mount::arming`'s `arm_addresses_accounts` (2026-09-15) **every `vike_model::VENUES` id
//! addresses a second account** — through its bridge's declaration now that every venue mounts
//! itself (that list until then) — so no roster venue can produce the block, so
//! `unaddressable_accounts_text` answers `None` for every input a mount can build. The assertion
//! passed for a reason that had stopped existing — and its module doc claimed it was "doing MORE
//! work than it was", which is the opposite of true. A test that cannot fail is worse than no test:
//! it reports coverage that is not there.
//!
//! What was lost by replacing it is nothing: the retirement it silently depended on is PINNED by
//! name in `crates/vike-tradehub/tests/unaddressable_account_warning.rs`'s
//! `no_roster_venue_is_unaddressable_any_more` (which drives the real projection over the whole
//! roster and fails if a venue re-enters that class), and that file also drives the message's TEXT
//! against a planted row.
//!
//! # What this file proves instead, and why it is the same shape
//!
//! The refusal an operator actually meets on a two-account box is now the ONE-SIDECAR decline:
//! dukascopy opens one JForex sidecar per process, `vike_mount`'s generic exclusive rule (`holder`
//! in `crates/vike-mount/src/exclusive.rs`) decides from the POLICY which account gets it, and
//! every other dukascopy account — **including the DEFAULT one** — stays paper. Both halves are
//! asserted here, in one process, over the real mount:
//!
//! * a box that names no second account is told NOTHING (and its default account is not declined);
//! * a box that names one is told, at the account that lost, WHICH account holds the sidecar.
//!
//! Silence and speech in ONE test function deliberately: `log_capture::install` may be called once
//! per process (it sets the global `tracing` default — see that module for why a thread-local one
//! was rejected), so two test functions in this binary could not both have a collector.
//!
//! ⚠ **Neither phase can spawn a JVM.** Phase 1's store holds no dukascopy credentials, so the
//! bridge's mount stays paper before `DukascopyExecutionClient::spawn`; phase 2 mounts the account
//! that DECLINES, and the fold answers a decline without asking the bridge to mount at all
//! (`crates/vike-mount/src/contract.rs`'s `contract_parts`). That is what makes a real-mount
//! assertion about this venue possible at all in a test binary.
//!
//! Moved from `crates/vike-mount/tests/` when the venue mount contract finished
//! (docs/decisions/0096): it drives `vike-mount`'s public fold with real venue ids, which only a
//! crate holding the registry can — `vike-tradehub` since the 2026-09-29 amendment. Default build
//! only: `vike-mount`'s registry carried ibkr, fxcm and polymarket `FeatureAbsent` in every build
//! and these assertions were written against that; each venue's feature-on half is its own
//! `crates/vike-tradehub/tests/ibkr_mount.rs`, `crates/vike-tradehub/tests/fxcm_mount.rs` or
//! `crates/vike-tradehub/tests/polymarket_mount.rs`. That crate-level `#![cfg]` also makes this its
//! own test binary rather than a `daemon` member.
#![cfg(not(any(feature = "ibkr", feature = "polymarket", feature = "fxcm")))]

mod log_capture;

use std::collections::{HashMap, HashSet};

use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::Account;
use vike_tradehub::registry::REGISTRY;

/// The live two-account shape: two `(dukascopy, demo)` rows with the books an operator wrote against
/// them — the DEFAULT account (unlabelled) and, when `named`, a second row whose label is its book,
/// the active labelled row that names and arms that account (decision 0119). Two accounts at one
/// tier are told apart by their LABELS (decision 0156 item 6); a credential belongs to its account's
/// row, so `named` rows also HOLD their `LOGIN` / `PASSWORD` fields.
fn policy(named: bool) -> vike_mount::MountPolicy {
    let row = |id: i64, book: &str, labelled: bool| Account {
        id,
        venue: "dukascopy".to_string(),
        tier: vike_model::AccountTier::Demo,
        label: labelled.then(|| book.to_string()),
        venue_account_id: Some(book.to_string()),
        parent_id: None,
        active: true,
        last_verified_at: None,
        max_exposure: None,
    };
    let base = vike_mount::MountPolicy::default().with_account_row(row(7, "3709890", false));
    if !named {
        return base;
    }
    let second = AccountLabel::parse("3716974").expect("a legal label");
    base.with_account_row(row(8, "3716974", true))
        .with_credentials(
            "dukascopy",
            &AccountLabel::Default,
            vike_model::AccountTier::Demo,
            &[("LOGIN", "fake-login-1"), ("PASSWORD", "fake-pass-1")],
        )
        .with_credentials(
            "dukascopy",
            &second,
            vike_model::AccountTier::Demo,
            &[("LOGIN", "fake-login-2"), ("PASSWORD", "fake-pass-2")],
        )
}

/// Mount ONE account of dukascopy for real, and report the live set it wrote.
fn mount(
    account: &AccountLabel,
    vars: &HashMap<String, String>,
    policy: &vike_mount::MountPolicy,
) -> HashSet<String> {
    let (events, _rx) = vike_exec::event_channel(16);
    let mut live: HashSet<String> = HashSet::new();
    let mut env = vike_mount::MountEnv::new(REGISTRY, vars, &events, &mut live);
    env.policy = Some(policy);
    // Every mount here ends NOT RUNNING (decision 0156): no credentials, or a declined sidecar.
    match vike_mount::make_engine_for_account(&mut env, "dukascopy", "EURUSD", account, &[]) {
        Err(vike_mount::MountError::NotRunning(_)) => {}
        Err(other) => panic!("only a NOT RUNNING answer is expected: {other}"),
        Ok(_) => panic!("no dukascopy account may run in this test"),
    }
    live
}

#[test]
fn the_default_account_is_silent_until_another_account_takes_the_sidecar() {
    let log = log_capture::install();

    // VACUITY CONTROL — an empty capture and a silent mount are different things, and without this
    // they print identically.
    tracing::warn!("planted control event");
    assert!(
        log_capture::lines(&log).iter().any(|l| l.contains("planted control event")),
        "the capture harness recorded nothing at all: {:#?}",
        log_capture::lines(&log)
    );

    // PHASE 1 — a box whose readable `account` table holds dukascopy's one unlabelled `demo` row
    // (the DEFAULT account) and NO second account named anywhere. It holds no dukascopy credentials, so the mount self-gates
    // into paper without going near a JVM, and nothing about a second account may be said to it.
    let single = policy(false);
    let live = mount(&AccountLabel::Default, &HashMap::new(), &single);
    assert!(live.is_empty(), "no credentials, so no live arm");
    let lines = log_capture::lines(&log);
    assert!(
        !lines.iter().any(|l| l.contains("A SECOND ACCOUNT IS NAMED")),
        "a single-account box must be told nothing about second accounts: {lines:#?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("holds it")),
        "…and its DEFAULT account must not be told another account took the sidecar, because none \
         did: {lines:#?}"
    );
    let before = lines.len();

    // PHASE 2 — the same box, with the second row labelled by its book `3716974` (an active labelled
    // row) and BOTH accounts holding their credentials. The labelled account takes the sidecar, the DEFAULT account declines, and
    // the decline is said out loud at the account that lost it.
    let two = policy(true);
    let live = mount(&AccountLabel::Default, &HashMap::new(), &two);
    assert!(
        live.is_empty(),
        "the DEFAULT account declined, so it arms nothing — even with its own credentials present"
    );

    let said: Vec<String> = log_capture::lines(&log).split_off(before);
    let decline = said
        .iter()
        .find(|l| l.contains("holds it"))
        .unwrap_or_else(|| panic!("the declining account must be told why: {said:#?}"));
    assert!(decline.contains("3716974"), "…and WHICH account holds it: {decline}");
    assert!(decline.contains("PAPER"), "…and what this account is left on: {decline}");
    assert!(
        decline.contains("[ERROR]"),
        "…at a level an operator sees: an armed account silently becoming paper is the failure \
         this line exists to prevent: {decline}"
    );
}
