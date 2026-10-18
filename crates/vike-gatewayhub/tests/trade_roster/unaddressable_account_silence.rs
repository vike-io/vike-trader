//! **What a start says about a SECOND account — and, on a box that has none, what it must not say.**
//!
//! ⚠ **This file's original assertion could no longer fail, and it has been replaced rather than
//! left standing.** It drove `make_engine_accounts` on a single-account box and asserted the
//! process-wide unaddressable-account report stayed silent. That report is keyed on
//! `vike_config::ArmingBlock::NoAccountSupport`, and since dukascopy joined
//! `vike_tradehub_core::arming`'s `arm_addresses_accounts` (2026-09-15) **every `vike_model::VENUES` id
//! addresses a second account** — through its bridge's declaration now that every venue starts
//! itself (that list until then) — so no roster venue can produce the block, so
//! `unaddressable_accounts_text` answers `None` for every input a start can build. The assertion
//! passed for a reason that had stopped existing — and its module doc claimed it was "doing MORE
//! work than it was", which is the opposite of true. A test that cannot fail is worse than no test:
//! it reports coverage that is not there.
//!
//! What was lost by replacing it is nothing: the retirement it silently depended on is PINNED by
//! name in `crates/vike-gatewayhub/tests/trade_roster/registry_shape/addressing.rs`'s
//! `no_roster_venue_is_unaddressable_any_more` (which drives the real projection over the whole
//! roster and fails if a venue re-enters that class), and `crates/vike-tradehub/tests/unaddressable_account_warning.rs` drives the
//! message's TEXT against a planted row.
//!
//! # What this file proves instead, and why it is the same shape
//!
//! The refusal an operator actually meets on a two-account box is now the ONE-SIDECAR decline:
//! dukascopy opens one JForex sidecar per process, `vike_tradehub_core`'s generic exclusive rule (`holder`
//! in `crates/vike-gatewayhub/src/trade/adapters/exclusive.rs`) decides from the POLICY which account gets it, and
//! every other dukascopy account — **including the DEFAULT one** — stays paper. Both halves are
//! asserted here, in one process, over the real start:
//!
//! * a box that names no second account is told NOTHING (and its default account is not declined);
//! * a box that names one is told, at the account that lost, WHICH account holds the sidecar.
//!
//! Silence and speech in ONE test function: both phases read a scoped capture
//! (`vike_log::capture::captured`, the events of the calling thread, which is where the fold logs).
//!
//! ⚠ **Neither phase can spawn a JVM.** Phase 1's store holds no dukascopy credentials, so the
//! bridge's start stays paper before `DukascopyExecutionClient::spawn`; phase 2 starts the account
//! that DECLINES, and the fold answers a decline without asking the bridge to start at all
//! (`crates/vike-gatewayhub/src/trade/adapters/contract.rs`'s `contract_parts`). That is what makes a real-mount
//! assertion about this venue possible at all in a test binary.
//!
//! Moved from `crates/vike-tradehub-core/tests/` when the venue-adapter contract finished
//! (docs/decisions/0096): it drives `vike-tradehub-core`'s public fold with real venue ids, which only a
//! crate holding the registry can — `vike-tradehub` since the 2026-09-29 amendment. Default build
//! only: `vike-tradehub-core`'s registry carried ibkr, fxcm and polymarket `FeatureAbsent` in every build
//! and these assertions were written against that; each venue's feature-on half is its own
//! `crates/vike-gatewayhub/tests/trade_roster/ibkr_adapter.rs`, `crates/vike-gatewayhub/tests/trade_roster/fxcm_adapter.rs` or
//! `crates/vike-gatewayhub/tests/trade_roster/polymarket_adapter.rs`. That crate-level `#![cfg]` also makes this its
//! own test binary rather than a `daemon` member.
#![cfg(not(any(feature = "ibkr", feature = "polymarket", feature = "fxcm")))]

// The in-process backend the roster tests drive the fold through (wave 2, W9a).
#[path = "support/local_backend.rs"]
mod local;

use std::collections::{HashMap, HashSet};

use vike_gatewayhub::registry::trade::REGISTRY;
use vike_log::capture::{CapturedEvent, captured};
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::Account;

/// Every captured event rendered `[LEVEL] target: message`, the way an operator reads the log.
fn lines(events: &[CapturedEvent]) -> Vec<String> {
    events.iter().map(|e| format!("[{}] {}: {}", e.level, e.target, e.message)).collect()
}

/// The live two-account shape: two `(dukascopy, demo)` rows with the books an operator wrote against
/// them — the DEFAULT account (unlabelled) and, when `named`, a second row whose label is its book,
/// the active labelled row that names and arms that account (decision 0119). Two accounts at one
/// tier are told apart by their LABELS (decision 0156 item 6); a credential belongs to its account's
/// row, so `named` rows also HOLD their `LOGIN` / `PASSWORD` fields.
fn policy(named: bool) -> vike_tradehub_core::AdapterPolicy {
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
        leverage: None,
    };
    let base =
        vike_tradehub_core::AdapterPolicy::default().with_account_row(row(7, "3709890", false));
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

/// Start ONE account of dukascopy for real, and report the live set it wrote.
fn start_adapter(
    account: &AccountLabel,
    vars: &HashMap<String, String>,
    policy: &vike_tradehub_core::AdapterPolicy,
) -> HashSet<String> {
    let (events, _rx) = vike_exec::event_channel(16);
    let mut live: HashSet<String> = HashSet::new();
    let rig = crate::local::Rig::new(REGISTRY, vars, Some(policy));
    let mut env = rig.env(&events, &mut live);
    env.policy = Some(policy);
    // Every start here ends NOT RUNNING (decision 0156): no credentials, or a declined sidecar.
    match vike_tradehub_core::make_engine_for_account(&mut env, "dukascopy", "EURUSD", account, &[])
    {
        Err(vike_tradehub_core::EngineError::NotRunning(_)) => {}
        Err(other) => panic!("only a NOT RUNNING answer is expected: {other}"),
        Ok(_) => panic!("no dukascopy account may run in this test"),
    }
    live
}

#[test]
fn the_default_account_is_silent_until_another_account_takes_the_sidecar() {
    // VACUITY CONTROL — an empty capture and a silent start are different things, and without this
    // they print identically.
    let ((), planted) = captured(|| tracing::warn!("planted control event"));
    assert!(
        lines(&planted).iter().any(|l| l.contains("planted control event")),
        "the capture harness recorded nothing at all: {planted:#?}"
    );

    // PHASE 1 — a box whose readable `account` table holds dukascopy's one unlabelled `demo` row
    // (the DEFAULT account) and NO second account named anywhere. It holds no dukascopy credentials, so the start self-gates
    // into paper without going near a JVM, and nothing about a second account may be said to it.
    let single = policy(false);
    let (live, events) =
        captured(|| start_adapter(&AccountLabel::Default, &HashMap::new(), &single));
    assert!(live.is_empty(), "no credentials, so no live arm");
    let said = lines(&events);
    assert!(
        !said.iter().any(|l| l.contains("A SECOND ACCOUNT IS NAMED")),
        "a single-account box must be told nothing about second accounts: {said:#?}"
    );
    assert!(
        !said.iter().any(|l| l.contains("holds it")),
        "…and its DEFAULT account must not be told another account took the sidecar, because none \
         did: {said:#?}"
    );

    // PHASE 2 — the same box, with the second row labelled by its book `3716974` (an active labelled
    // row) and BOTH accounts holding their credentials. The labelled account takes the sidecar, the DEFAULT account declines, and
    // the decline is said out loud at the account that lost it.
    let two = policy(true);
    let (live, events) = captured(|| start_adapter(&AccountLabel::Default, &HashMap::new(), &two));
    assert!(
        live.is_empty(),
        "the DEFAULT account declined, so it arms nothing — even with its own credentials present"
    );

    let said = lines(&events);
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
