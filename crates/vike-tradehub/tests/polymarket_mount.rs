//! POLYMARKET through the REAL registry — the tests that drive `vike-tradehub-core`'s PUBLIC fold with the
//! real venue id. They ran in `vike-tradehub-core` under its own `polymarket` feature until the venue start
//! contract moved the start into the bridge and its registry row here, to the crate that already
//! owned the bridge edge (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md,
//! amended 2026-09-29): `vike-tradehub-core`'s own registry cannot name the bridge, and a bridge cannot call
//! `vike-tradehub-core` without a dependency cycle, so this crate — which sees both — is where they run.
//! Their assertions are the ones they had there.
//!
//! Compiled only under this crate's `polymarket` feature
//! (`ci.yml`'s `tradehub-all` job, `just tradehub-all`); without it the registry carries
//! polymarket `FeatureAbsent`. That crate-level `#![cfg]` is why this is its own test binary rather
//! than a `daemon` member. Every test is OFFLINE: each gate pinned here returns before the key
//! derivation, the geoblock pre-flight and the L2 round-trip. The bridge's own gates — its resolve,
//! its recon-only outcome, its declaration — are
//! `crates/bridges/polymarket/src/exec_plane/adapter_contract_tests.rs`'s.
#![cfg(feature = "polymarket")]

use std::collections::{HashMap, HashSet};

use vike_config::{ArmingBlock, VenueArming};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_polymarket::{POLY_EXEC_ENV, POLY_RECONCILE_ENV};
use vike_secrets::account_credentials::{AccountCredentials, CredentialFields};
use vike_tradehub::registry::REGISTRY;
use vike_tradehub_core::book_identity::effective_book;
use vike_tradehub_core::preflight::ServerTimeGap;
use vike_tradehub_core::{AdapterPolicy, VenueRow};

const VENUE: &str = "polymarket";

/// A syntactically valid secp256k1 key that is NOT a real account — the start's gates return
/// before anything is ever signed with it, which is the property these tests assert.
const POLY_TEST_KEY: &str = "0xc85ef7d79691fe79573b1a7064c19c1a9819ebdbd1faaab1a8ec92344438aaf4";

fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

/// Every start test below wants the polymarket START to actually run, so each one plants an active
/// `account` row for the DEFAULT account — at `live` unless it says otherwise, the tier that venue's
/// exec requires, since it has no testnet. Without it the start returns at the account table and
/// the venue's own double gate, which is what these tests exist to pin, is never consulted.
fn armed(mode: AccountTier) -> AdapterPolicy {
    AdapterPolicy::default().with_account(VENUE, &AccountLabel::Default, mode)
}

/// [`armed`] with the account holding `fields` (a credential belongs to its account, decision 0156
/// item 6).
fn armed_holding(mode: AccountTier, fields: &[(&str, &str)]) -> AdapterPolicy {
    AdapterPolicy::default().with_credentials(VENUE, &AccountLabel::Default, mode, fields)
}

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// MOVED from `vike-tradehub-core`'s fee-schedule tests. Feature-on coverage of the polymarket start, the
/// OFFLINE half: BOTH gates are the FIRST things read, so creds present + both unset ⇒ the account
/// does NOT RUN (`ExecFlagUnset`, decision 0156: the exec flag is a stop) — no reconcile handle, no
/// exec client AND no network call (the L2 `/auth/derive-api-key` round-trip lives
/// behind them). This is the byte-identical-to-a-default-build case the double gate exists to
/// guarantee.
///
/// ⚠ **Since S2 this is also the SECOND gate's assertion, and the wording matters because the
/// first draft of it was wrong.** `vike_tradehub::reconcile_config::reconcile_gate` turns the master
/// gate ON for every start that arms a live venue account. That default DOES reach Polymarket —
/// it is the same driver, started over the same `recon_clients` vector — so it is NOT true that
/// this venue "did not change": what changed is that its OUTER gate is now supplied by the box
/// rather than typed by a person, leaving `flags.poly_reconcile` as the one remaining act. It is
/// still one act more than any other venue needs, and it is still what this test pins: the
/// `recon_enabled: true` below passes the master gate in its DEFAULT-ON state and the test
/// demands `recon.is_none()` anyway, so a PR that dropped the venue gate as redundant reddens
/// here. Why the venue gate must survive the default:
/// `crates/bridges/polymarket/CLAUDE.md` (reconciling a live Polymarket account against a paper
/// engine, and the venue's absence of any testnet), and
/// `docs/decisions/0043-reconcile-is-on-by-default-under-quarantine.md` for what the owner is
/// being asked to ratify.
///
/// (Its old skip for a gate exported in the process environment is gone: since decision 0095 both
/// gates read the credential map alone, and this map carries neither, so the skip could not fire.)
#[test]
fn polymarket_without_the_gates_is_inert_and_offline() {
    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = HashSet::new();
    let vars = vars(&[("POLY_PRIVATE_KEY", POLY_TEST_KEY)]);
    let policy = armed(AccountTier::Live);
    let mut env = vike_tradehub_core::AdapterEnv::new(REGISTRY, &vars, &tx, &mut live);
    // `recon_enabled: true` on purpose: this start reads BOTH gates
    // (`poly_recon_wanted`), and passing the master one in its default-on state is what makes
    // the assertion below about the VENUE gate rather than about a start that was off anyway.
    env.recon_enabled = true;
    env.policy = Some(&policy);
    let stopped = not_running(vike_tradehub_core::make_engine(
        &mut env,
        VENUE,
        "71321045679252212594626385532706912750332728571942532289631379312455583992563",
    ));
    assert_eq!(
        stopped.block,
        ArmingBlock::ExecFlagUnset,
        "flags.poly_exec off is a STOP: the account does not run, and no reconcile handle (and \
         no network call) is built for an engine that does not exist (decision 0156)"
    );
    assert!(live.is_empty(), "flags.poly_exec off → never marked live");
}

/// MOVED. Recon gate ON but creds ABSENT ⇒ still no handle, still no network call (the factory's
/// own absent-credentials-is-the-live-gate return precedes its `ensure_l2` round-trip). Proves the
/// two gates compose in both orders, offline.
#[test]
fn polymarket_with_the_recon_gate_but_no_creds_stays_inert() {
    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = HashSet::new();
    let vars = vars(&[(POLY_RECONCILE_ENV, "1")]);
    let policy = armed(AccountTier::Live);
    let mut env = vike_tradehub_core::AdapterEnv::new(REGISTRY, &vars, &tx, &mut live);
    env.recon_enabled = true;
    env.policy = Some(&policy);
    let stopped = not_running(vike_tradehub_core::make_engine(&mut env, VENUE, "0"));
    assert_eq!(
        stopped.block,
        ArmingBlock::ExecFlagUnset,
        "recon gate on, exec flag off: the account does not run, so nothing reconciles"
    );
    assert!(live.is_empty());
}

/// MOVED. The EXEC gate's twin: `flags.poly_exec` on with NO `POLY_PRIVATE_KEY` ⇒ no live client,
/// the account does not run (decision 0156), and — the part that matters for CI — no network call, because
/// `live_runtime_for_account` returns on absent credentials before its `ensure_l2` round-trip.
#[test]
fn polymarket_with_the_exec_gate_but_no_creds_does_not_run_and_stays_offline() {
    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = HashSet::new();
    let vars = vars(&[(POLY_EXEC_ENV, "1")]);
    let policy = armed(AccountTier::Live);
    let mut env = vike_tradehub_core::AdapterEnv::new(REGISTRY, &vars, &tx, &mut live);
    env.recon_enabled = true;
    env.policy = Some(&policy);
    let stopped = not_running(vike_tradehub_core::make_engine(&mut env, VENUE, "0"));
    assert_eq!(stopped.block, ArmingBlock::NoCredentials, "no key: the account does not run");
    assert!(live.is_empty(), "flags.poly_exec on but no key → never marked live");
}

/// MOVED. …and with an UNUSABLE key PRESENT: CONTRACT CHANGED by the pre-connect refusal (#817
/// residual). Key material the operator wrote + the explicit `flags.poly_exec` flag IS declared
/// live intent (`would_go_live`'s polymarket row, same stance as hyperliquid's), so with
/// no risk budget the start now REFUSES — before the factory would even try (and fail) the
/// EOA derivation — instead of silently falling back to paper as it did before. Still
/// entirely offline: the refusal precedes any network. The no-key case above keeps the paper
/// fallback (a flag with no key can never start live, so it is not intent).
#[test]
fn polymarket_exec_gate_with_a_bad_key_refuses_preconnect_without_budget() {
    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = HashSet::new();
    let vars = vars(&[(POLY_EXEC_ENV, "1")]);
    let policy = armed_holding(AccountTier::Live, &[("PRIVATE_KEY", "not-a-key")]);
    let mut env = vike_tradehub_core::AdapterEnv::new(REGISTRY, &vars, &tx, &mut live);
    env.recon_enabled = true;
    env.policy = Some(&policy);
    let err = match vike_tradehub_core::make_engine(&mut env, VENUE, "0") {
        Err(e) => e,
        Ok(_) => panic!("key present + flags.poly_exec + no budget must refuse pre-connect"),
    };
    let msg = format!("{err}");
    assert!(msg.contains("polymarket"), "must name the venue: {msg}");
    assert!(msg.contains("max_notional_per_order") && msg.contains("max_total_exposure"));
    assert!(live.is_empty(), "the refusal precedes the arm — nothing recorded live");

    // …and the ACCOUNT'S TIER is what decides whether any of that is reached at all. For a
    // `demo` account the SAME configuration is not live intent: polymarket has no testnet, so `demo`
    // names a tier that does not exist and its exec arm refuses rather than arming the only
    // tier there is (REAL money on Polygon mainnet). It does not run, offline (decision 0156).
    let mut demoted = HashSet::new();
    let demo_policy = armed_holding(AccountTier::Demo, &[("PRIVATE_KEY", "not-a-key")]);
    let mut env = vike_tradehub_core::AdapterEnv::new(REGISTRY, &vars, &tx, &mut demoted);
    env.recon_enabled = true;
    env.policy = Some(&demo_policy);
    let stopped = not_running(vike_tradehub_core::make_engine(&mut env, VENUE, "0"));
    assert_eq!(
        stopped.block,
        ArmingBlock::LiveOnlyArm,
        "no testnet: a `demo` account does not run"
    );
    assert!(demoted.is_empty(), "a `demo` account must not arm a mainnet-only venue");
}

/// MOVED from `vike-tradehub-core`'s pre-connect tests — `venues_without_a_live_arm_probe_false_even_with_creds`'
/// feature-on half: intent needs BOTH factory gates, the explicit `flags.poly_exec` flag AND key
/// material. The flag ALONE can never start live (no key ⇒ the factory returns before any network)
/// and must stay paper-probed; the flag with a key is intent.
#[test]
fn the_exec_flag_alone_is_not_intent_and_the_flag_with_a_key_is() {
    let flag = vars(&[("POLY_EXEC", "1")]);
    assert!(!vike_tradehub_core::would_go_live(REGISTRY, VENUE, &flag, AccountCredentials::none()));
    let held = CredentialFields::from_pairs([("PRIVATE_KEY", "0xkey")]);
    assert!(vike_tradehub_core::would_go_live(
        REGISTRY,
        VENUE,
        &flag,
        AccountCredentials::new(Some(1), Some(&held), None)
    ));
}

/// MOVED — the feature-on half of `vike-tradehub-core`'s
/// `a_venue_this_build_cannot_start_says_so_instead_of_blaming_credentials`: an empty store's first
/// missing conjunct for a `live` account is the exec flag, and a `demo` account is refused for the
/// tier that does not exist.
#[test]
fn an_empty_store_names_the_exec_flag_and_a_demo_ceiling_names_the_missing_tier() {
    let default_row = |mode: AccountTier| -> VenueArming {
        vike_tradehub_core::venue_account_arming(
            REGISTRY,
            VENUE,
            &HashMap::new(),
            Some(&armed(mode)),
        )
        .into_iter()
        .find(VenueArming::is_default_account)
        .expect("the default account's row")
    };
    assert_eq!(
        default_row(AccountTier::Live).block,
        ArmingBlock::ExecFlagUnset,
        "with the feature ON, an empty map's first missing conjunct is POLY_EXEC"
    );
    let demo = default_row(AccountTier::Demo);
    assert_eq!(
        (demo.effective, demo.block),
        (None, ArmingBlock::LiveOnlyArm),
        "polymarket runs no testnet, so a `demo` account does not run"
    );
}

/// MOVED from `vike-tradehub-core`'s `tests/account_credential_isolation.rs`, its polymarket case, over
/// the REAL projection: the fixture arms the default account; a labelled account reads none of the
/// default account's keys; a labelled key set arms the labelled account and only it; a second
/// account's key perturbs nothing about the first account's row; and an unconfigured labelled
/// account reads like an unconfigured default one. (Its half-written row skips a single-key venue,
/// and this is one.) The exec flag is DEPLOYMENT-wide — it names no wallet — so every store carries
/// it unlabelled.
#[test]
fn a_labelled_polymarket_account_reads_only_its_own_key() {
    // Two `live` rows (the DEFAULT account and ALT), each holding `PRIVATE_KEY` or nothing.
    let policy = |default: Option<&str>, labelled: Option<&str>| -> AdapterPolicy {
        let mut p = armed(AccountTier::Live).with_account(VENUE, &alt(), AccountTier::Live);
        for (label, value) in [(AccountLabel::Default, default), (alt(), labelled)] {
            if let Some(value) = value {
                p = p.with_credentials(VENUE, &label, AccountTier::Live, &[("PRIVATE_KEY", value)]);
            }
        }
        p
    };
    let flag = vars(&[("POLY_EXEC", "1")]);
    let row = |label: &AccountLabel, policy: &AdapterPolicy| -> VenueArming {
        vike_tradehub_core::venue_account_arming(REGISTRY, VENUE, &flag, Some(policy))
            .into_iter()
            .find(|r| r.label == *label)
            .unwrap_or_else(|| panic!("polymarket: no row for {label}"))
    };
    let default = AccountLabel::Default;

    let default_keys = policy(Some("0xkey"), None);
    let armed_default = row(&default, &default_keys);
    assert_eq!(
        armed_default.effective,
        Some(AccountTier::Live),
        "polymarket: the fixture key set must ARM the default account, or this test proves \
         nothing ({:?})",
        armed_default.block
    );
    let leaked = row(&alt(), &default_keys);
    assert_eq!(
        (leaked.effective, leaked.block),
        (None, ArmingBlock::NoCredentials),
        "polymarket armed a LABELLED account off the DEFAULT account's keys — a second live client \
         signing for the first account"
    );

    let alt_keys = policy(None, Some("0xkey"));
    assert_eq!(
        row(&alt(), &alt_keys).effective,
        Some(AccountTier::Live),
        "polymarket: labelled account NOT armed by its own keys"
    );
    let lost = row(&default, &alt_keys);
    assert_eq!(
        (lost.effective, lost.block),
        (None, ArmingBlock::NoCredentials),
        "polymarket: the DEFAULT account armed off a LABELLED key set"
    );

    let beside = policy(Some("0xkey"), Some("0xkey-second-account"));
    assert_eq!(
        row(&default, &default_keys),
        row(&default, &beside),
        "a second account's credentials changed the FIRST account's row"
    );

    let empty = policy(None, None);
    assert_eq!(
        row(&alt(), &empty).block,
        row(&default, &empty).block,
        "polymarket: an unconfigured labelled account must read like an unconfigured default one"
    );
}

/// MOVED from `vike-tradehub-core`'s `tests/book_identity_table.rs`: its polymarket tuples in
/// `every_named_row_resolves_from_its_own_keys` (the `Named` row resolves from the REAL key name,
/// through the real label grammar) and `polymarket_has_no_demo_tier_to_read` (a demo-resolved
/// account reads no key at all, which matches the start's `LiveOnlyArm` below `live`).
#[test]
fn the_polymarket_book_is_named_by_its_own_address_and_no_demo_tier_exists() {
    // The `ADDRESS` field names the book — a field carries no tier (decision 0156 item 6).
    let held = CredentialFields::from_pairs([("ADDRESS", "0xFunder")]);
    let creds = AccountCredentials::new(Some(1), Some(&held), None);
    assert_eq!(
        effective_book(REGISTRY, VENUE, AccountTier::Live, creds).as_deref(),
        Some("0xfunder"),
        "{VENUE}: the ADDRESS field must name the book"
    );
    // …and an account holding nothing has no book — the no-fallback rule every credential loader
    // in this workspace obeys.
    assert_eq!(
        effective_book(REGISTRY, VENUE, AccountTier::Live, AccountCredentials::none()),
        None,
        "{VENUE}: an account holding no ADDRESS names no book"
    );

    // polymarket has no demo tier, so a demo-resolved account is PAPER (`LiveOnlyArm`, asserted
    // above); the book of its row is the address it holds, and no start parks it. (The declaration
    // used to say "no demo tier" in a field of its own; a field carries no tier now.)
    assert_eq!(
        effective_book(REGISTRY, VENUE, AccountTier::Demo, creds).as_deref(),
        Some("0xfunder")
    );
}

/// The roster's one ORDER-affecting clock gap, answered through the real row: the assertion
/// `vike-tradehub-core`'s `a_declared_venue_with_orders_at_stake_reports_the_risk_not_a_shrug` made over
/// this row before its own registry carried the venue `FeatureAbsent`. Pure — no network.
#[test]
fn polymarket_answers_the_at_risk_clock_outcome_through_the_registry() {
    let Some(vike_bridge_core::venue_adapter::ClockDecl::NotWired {
        reason,
        unmeasured_risk: Some(at_stake),
    }) = vike_tradehub_core::server_time::clock_decl(REGISTRY, VENUE)
    else {
        panic!("polymarket's row declares its clock and the risk it leaves unmeasured");
    };
    let gap = vike_tradehub_core::server_time::venue_server_time_ms(
        REGISTRY,
        VENUE,
        &HashMap::new(),
        AccountTier::Demo,
    )
    .expect_err("declared venues never measure");
    assert_eq!(gap, ServerTimeGap::UnmeasuredRisk { reason, at_stake });
}

/// Under the feature the row is the BRIDGE's start — a `cfg` pair typed the wrong way round would
/// leave the daemon starting polymarket paper with no error.
#[test]
fn under_the_feature_the_registry_row_is_the_bridges_start() {
    match vike_tradehub_core::row_of(REGISTRY, VENUE) {
        Some(VenueRow::Adapter(m)) => assert_eq!(m.venue(), VENUE),
        other => panic!("polymarket's row under the `polymarket` feature is {other:?}"),
    }
}

/// The account a start reported NOT RUNNING (decision 0156), or a panic naming what came back.
fn not_running(
    out: Result<vike_tradehub_core::EngineAndRecon, vike_tradehub_core::EngineError>,
) -> vike_tradehub_core::NotRunning {
    match out {
        Err(vike_tradehub_core::EngineError::NotRunning(stopped)) => stopped,
        Err(other) => panic!("expected NOT RUNNING, got the refusal: {other}"),
        Ok((engine, _)) => panic!("expected NOT RUNNING, got an engine at {}", engine.tier),
    }
}
