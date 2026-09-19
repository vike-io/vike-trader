//! **What a mount says about a SECOND account — and, on a box that has none, what it must not say.**
//!
//! ⚠ **This file's original assertion could no longer fail, and it has been replaced rather than
//! left standing.** It drove `make_engine_accounts` on a single-account box and asserted the
//! process-wide unaddressable-account report stayed silent. That report is keyed on
//! `vike_config::ArmingBlock::NoAccountSupport`, and since dukascopy joined
//! `vike_mount::arming`'s `arm_addresses_accounts` (2026-09-15) **every `vike_model::VENUES` id is
//! in that list**, so no roster venue can produce the block, so `unaddressable_accounts_text`
//! answers `None` for every input a mount can build. The assertion passed for a reason that had
//! stopped existing — and its module doc claimed it was "doing MORE work than it was", which is the
//! opposite of true. A test that cannot fail is worse than no test: it reports coverage that is not
//! there.
//!
//! What was lost by replacing it is nothing: the retirement it silently depended on is PINNED by
//! name in `crates/vike-mount/tests/unaddressable_account_warning.rs`'s
//! `no_roster_venue_is_unaddressable_any_more` (which drives the real projection over the whole
//! roster and fails if a venue re-enters that class), and that file also drives the message's TEXT
//! against a planted row.
//!
//! # What this file proves instead, and why it is the same shape
//!
//! The refusal an operator actually meets on a two-account box is now the ONE-SIDECAR decline:
//! dukascopy opens one JForex sidecar per process, `vike_mount`'s `dukascopy` module decides from
//! the POLICY which account gets it, and every other dukascopy account — **including the DEFAULT
//! one** — stays paper. Both halves are asserted here, in one process, over the real mount:
//!
//! * a box that names no second account is told NOTHING (and its default account is not declined);
//! * a box that names one is told, at the account that lost, WHICH account holds the sidecar.
//!
//! Silence and speech in ONE test function deliberately: `common::install` may be called once per
//! process (it sets the global `tracing` default — see that module for why a thread-local one was
//! rejected), so two test functions in this binary could not both have a collector.
//!
//! ⚠ **Neither phase can spawn a JVM.** Phase 1's store holds no dukascopy credentials, so the arm
//! self-gates before `DukascopyExecutionClient::spawn`; phase 2 mounts the account that DECLINES,
//! and the decline is checked above the credential read. That is what makes a real-mount assertion
//! about this venue possible at all in a test binary.

mod common;

use std::collections::{HashMap, HashSet};

use vike_bridge_core::credentials::{Account, AccountKeys, Accounts};
use vike_config::{VenueMode, VenuePolicy};
use vike_dukascopy::DukascopyAccount;
use vike_model::account_keys::AccountLabel;

/// The live two-account shape: two unlabelled `(dukascopy, demo)` rows, told apart ONLY by the
/// credential-key family each owns, with the books an operator wrote against them.
///
/// ⚠ The prefixes come from the BRIDGE rather than being spelled here — a fixture that re-spelled
/// them could drift from the loader it describes.
fn directory() -> vike_mount::AccountDirectory {
    let row = |id: i64, book: &str| Account {
        id,
        venue: "dukascopy".to_string(),
        tier: "demo".to_string(),
        label: None,
        venue_account_id: Some(book.to_string()),
        parent_id: None,
        active: true,
        last_verified_at: None,
    };
    let keys = |prefix: &str| AccountKeys { prefixes: vec![prefix.to_string()], names: Vec::new() };
    vike_mount::AccountDirectory::from_rows(
        Accounts::Known(vec![row(7, "3709890"), row(8, "3716974")]),
        Some(
            [
                (7, keys(DukascopyAccount::Demo1.key_prefix())),
                (8, keys(DukascopyAccount::Demo2.key_prefix())),
            ]
            .into_iter()
            .collect(),
        ),
    )
}

fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

/// Mount ONE account of dukascopy for real, and report the live set it wrote.
fn mount(
    account: &AccountLabel,
    vars: &HashMap<String, String>,
    policy: &vike_mount::MountPolicy,
) -> HashSet<String> {
    let (events, _rx) = vike_exec::event_channel(16);
    let mut live: HashSet<String> = HashSet::new();
    let _ = vike_mount::make_engine_for_account(
        "dukascopy",
        "EURUSD",
        account,
        &[],
        vars,
        &events,
        &mut live,
        false,
        None,
        None,
        None,
        Some(policy),
    )
    .expect("a paper mount refuses nothing");
    live
}

#[test]
fn the_default_account_is_silent_until_another_account_takes_the_sidecar() {
    let log = common::install();

    // VACUITY CONTROL — an empty capture and a silent mount are different things, and without this
    // they print identically.
    tracing::warn!("planted control event");
    assert!(
        common::lines(&log).iter().any(|l| l.contains("planted control event")),
        "the capture harness recorded nothing at all: {:#?}",
        common::lines(&log)
    );

    // PHASE 1 — a box with a `demo` ceiling for dukascopy, an `account` table it can read, and NO
    // second account named anywhere. It holds no dukascopy credentials, so the arm self-gates into
    // paper without going near a JVM, and nothing about a second account may be said to it.
    let single = vike_mount::MountPolicy {
        venues: VenuePolicy::default().declare("dukascopy", VenueMode::Demo),
        accounts: directory(),
        ..Default::default()
    };
    let live = mount(&AccountLabel::Default, &HashMap::new(), &single);
    assert!(live.is_empty(), "no credentials, so no live arm");
    let lines = common::lines(&log);
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

    // PHASE 2 — the same box, with `policy.accounts.dukascopy.3716974 = "demo"` and BOTH credential
    // families present. The labelled account takes the sidecar, the DEFAULT account declines, and
    // the decline is said out loud at the account that lost it.
    let two = vike_mount::MountPolicy {
        venues: VenuePolicy::default().declare("dukascopy", VenueMode::Demo).declare_account(
            "dukascopy",
            &label("3716974"),
            VenueMode::Demo,
        ),
        accounts: directory(),
        ..Default::default()
    };
    let both_key_families = vars(&[
        ("DUKASCOPY_DEMO1_LOGIN", "fake-login-1"),
        ("DUKASCOPY_DEMO1_PASSWORD", "fake-pass-1"),
        ("DUKASCOPY_DEMO2_LOGIN", "fake-login-2"),
        ("DUKASCOPY_DEMO2_PASSWORD", "fake-pass-2"),
    ]);
    let live = mount(&AccountLabel::Default, &both_key_families, &two);
    assert!(
        live.is_empty(),
        "the DEFAULT account declined, so it arms nothing — even with its own credentials present"
    );

    let said: Vec<String> = common::lines(&log).split_off(before);
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

fn label(text: &str) -> AccountLabel {
    AccountLabel::parse(text).expect("a legal label")
}
