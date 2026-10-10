//! POLYMARKET through the REAL registry — the tests that drive `vike-mount`'s PUBLIC fold with the
//! real venue id. They ran in `vike-mount` under its own `polymarket` feature until the venue mount
//! contract moved the mount into the bridge and its registry row here, to the crate that already
//! owned the bridge edge (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md,
//! amended 2026-09-29): `vike-mount`'s own registry cannot name the bridge, and a bridge cannot call
//! `vike-mount` without a dependency cycle, so this crate — which sees both — is where they run.
//! Their assertions are the ones they had there.
//!
//! Compiled only under this crate's `polymarket` feature
//! (`bash scripts/ci_feature_suite.sh polymarket-stack`); without it the registry carries
//! polymarket `FeatureAbsent`. That crate-level `#![cfg]` is why this is its own test binary rather
//! than a `daemon` member. Every test is OFFLINE: each gate pinned here returns before the key
//! derivation, the geoblock pre-flight and the L2 round-trip. The bridge's own gates — its resolve,
//! its recon-only outcome, its declaration — are
//! `crates/bridges/polymarket/src/exec_plane/mount_contract_tests.rs`'s.
#![cfg(feature = "polymarket")]

#[path = "support/permissive_grid.rs"]
mod permissive_grid;

use std::collections::{HashMap, HashSet};

use vike_config::{ArmingBlock, VenueArming, VenueMode, VenuePolicy};
use vike_model::accounts::account_keys::{AccountLabel, account_key};
use vike_mount::book_identity::effective_book;
use vike_mount::preflight::ServerTimeGap;
use vike_mount::{MountPolicy, VenueRow};
use vike_polymarket::{POLY_EXEC_ENV, POLY_RECONCILE_ENV};
use vike_tradehub::registry::REGISTRY;

use permissive_grid::assert_permissive_grid;

const VENUE: &str = "polymarket";

/// A syntactically valid secp256k1 key that is NOT a real account — the mount's gates return
/// before anything is ever signed with it, which is the property these tests assert.
const POLY_TEST_KEY: &str = "0xc85ef7d79691fe79573b1a7064c19c1a9819ebdbd1faaab1a8ec92344438aaf4";

fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

/// Every mount test below wants the polymarket MOUNT to actually run, so each one arms the ceiling —
/// to `live` unless it says otherwise, the tier that venue's exec requires, since it has no testnet.
/// Without it the mount returns at the paper early return and the venue's own double gate, which is
/// what these tests exist to pin, is never consulted.
fn armed(mode: VenueMode) -> MountPolicy {
    MountPolicy { venues: VenuePolicy::default().declare(VENUE, mode), ..Default::default() }
}

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// MOVED from `vike-mount`'s fee-schedule tests. Feature-on coverage of the polymarket mount, the
/// OFFLINE half: BOTH gates are the FIRST things read, so creds present + both unset ⇒ no reconcile
/// handle, no exec client AND no network call (the L2 `/auth/derive-api-key` round-trip lives
/// behind them). This is the byte-identical-to-a-default-build case the double gate exists to
/// guarantee.
///
/// ⚠ **Since S2 this is also the SECOND gate's assertion, and the wording matters because the
/// first draft of it was wrong.** `vike_tradehub::reconcile_config::reconcile_gate` turns the master
/// gate ON for every mount that arms a live venue account. That default DOES reach Polymarket —
/// it is the same driver, mounted over the same `recon_clients` vector — so it is NOT true that
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
    let policy = armed(VenueMode::Live);
    let mut env = vike_mount::MountEnv::new(REGISTRY, &vars, &tx, &mut live);
    // `recon_enabled: true` on purpose: this mount reads BOTH gates
    // (`poly_recon_wanted`), and passing the master one in its default-on state is what makes
    // the assertion below about the VENUE gate rather than about a mount that was off anyway.
    env.recon_enabled = true;
    env.policy = Some(&policy);
    let (engine, recon) = vike_mount::make_engine(
        &mut env,
        VENUE,
        "71321045679252212594626385532706912750332728571942532289631379312455583992563",
    )
    .expect("stays paper (no exec gate) -> the refusal check must not fire");
    assert!(
        recon.is_none(),
        "flags.poly_reconcile off → no reconcile handle (and no network call)"
    );
    assert!(live.is_empty(), "flags.poly_exec off → exec stays PAPER, never marked live");
    assert_eq!(engine.fee_schedule, Some(vike_model::fee_schedule_for(VENUE)));
    assert_permissive_grid(VENUE, &engine.gate.limits);
}

/// MOVED. Recon gate ON but creds ABSENT ⇒ still no handle, still no network call (the factory's
/// own absent-credentials-is-the-live-gate return precedes its `ensure_l2` round-trip). Proves the
/// two gates compose in both orders, offline.
#[test]
fn polymarket_with_the_recon_gate_but_no_creds_stays_inert() {
    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = HashSet::new();
    let vars = vars(&[(POLY_RECONCILE_ENV, "1")]);
    let policy = armed(VenueMode::Live);
    let mut env = vike_mount::MountEnv::new(REGISTRY, &vars, &tx, &mut live);
    env.recon_enabled = true;
    env.policy = Some(&policy);
    let (_engine, recon) = vike_mount::make_engine(&mut env, VENUE, "0")
        .expect("stays paper (no exec gate) -> the refusal check must not fire");
    assert!(recon.is_none(), "gate on but no POLY_PRIVATE_KEY → reconcile-inert");
    assert!(live.is_empty());
}

/// MOVED. The EXEC gate's twin: `flags.poly_exec` on with NO `POLY_PRIVATE_KEY` ⇒ no live client,
/// venue stays paper, and — the part that matters for CI — no network call, because
/// `live_mount_for_account` returns on absent credentials before its `ensure_l2` round-trip.
#[test]
fn polymarket_with_the_exec_gate_but_no_creds_stays_paper_and_offline() {
    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = HashSet::new();
    let vars = vars(&[(POLY_EXEC_ENV, "1")]);
    let policy = armed(VenueMode::Live);
    let mut env = vike_mount::MountEnv::new(REGISTRY, &vars, &tx, &mut live);
    env.recon_enabled = true;
    env.policy = Some(&policy);
    let (engine, recon) = vike_mount::make_engine(&mut env, VENUE, "0")
        .expect("no key -> stays paper -> the refusal check must not fire");
    assert!(live.is_empty(), "flags.poly_exec on but no key → paper, never marked live");
    assert!(recon.is_none());
    assert_eq!(engine.fee_schedule, Some(vike_model::fee_schedule_for(VENUE)));
}

/// MOVED. …and with an UNUSABLE key PRESENT: CONTRACT CHANGED by the pre-connect refusal (#817
/// residual). Key material the operator wrote + the explicit `flags.poly_exec` flag IS declared
/// live intent (`would_mount_live`'s polymarket row, same stance as hyperliquid's), so with
/// no risk budget the mount now REFUSES — before the factory would even try (and fail) the
/// EOA derivation — instead of silently falling back to paper as it did before. Still
/// entirely offline: the refusal precedes any network. The no-key case above keeps the paper
/// fallback (a flag with no key can never mount live, so it is not intent).
#[test]
fn polymarket_exec_gate_with_a_bad_key_refuses_preconnect_without_budget() {
    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = HashSet::new();
    let vars = vars(&[(POLY_EXEC_ENV, "1"), ("POLY_PRIVATE_KEY", "not-a-key")]);
    let policy = armed(VenueMode::Live);
    let mut env = vike_mount::MountEnv::new(REGISTRY, &vars, &tx, &mut live);
    env.recon_enabled = true;
    env.policy = Some(&policy);
    let err = match vike_mount::make_engine(&mut env, VENUE, "0") {
        Err(e) => e,
        Ok(_) => panic!("key present + flags.poly_exec + no budget must refuse pre-connect"),
    };
    let msg = format!("{err}");
    assert!(msg.contains("polymarket"), "must name the venue: {msg}");
    assert!(msg.contains("max_notional_per_order") && msg.contains("max_total_exposure"));
    assert!(live.is_empty(), "the refusal precedes the arm — nothing recorded live");

    // …and the ARMING CEILING is what decides whether any of that is reached at all. Under
    // `demo` the SAME configuration is not live intent: polymarket has no testnet, so `demo`
    // names a tier that does not exist and its exec arm refuses rather than arming the only
    // tier there is (REAL money on Polygon mainnet). Paper, offline, and no refusal to start.
    let mut demoted = HashSet::new();
    let demo_policy = armed(VenueMode::Demo);
    let mut env = vike_mount::MountEnv::new(REGISTRY, &vars, &tx, &mut demoted);
    env.recon_enabled = true;
    env.policy = Some(&demo_policy);
    let (_engine, recon) = vike_mount::make_engine(&mut env, VENUE, "0")
        .expect("a demo-capped polymarket is PAPER, and paper never refuses to start");
    assert!(demoted.is_empty(), "a `demo` ceiling must not arm a mainnet-only venue");
    assert!(recon.is_none(), "POLY_RECONCILE is unset here, so nothing reconciles either");
}

/// MOVED from `vike-mount`'s pre-connect tests — `venues_without_a_live_arm_probe_false_even_with_creds`'
/// feature-on half: intent needs BOTH factory gates, the explicit `flags.poly_exec` flag AND key
/// material. The flag ALONE can never mount live (no key ⇒ the factory returns before any network)
/// and must stay paper-probed; the flag with a key is intent.
#[test]
fn the_exec_flag_alone_is_not_intent_and_the_flag_with_a_key_is() {
    assert!(!vike_mount::would_mount_live(REGISTRY, VENUE, &vars(&[("POLY_EXEC", "1")])));
    assert!(vike_mount::would_mount_live(
        REGISTRY,
        VENUE,
        &vars(&[("POLY_EXEC", "1"), ("POLY_PRIVATE_KEY", "0xkey")])
    ));
}

/// MOVED — the feature-on half of `vike-mount`'s
/// `a_venue_this_build_cannot_mount_says_so_instead_of_blaming_credentials`: an empty store's first
/// missing conjunct under `live` is the exec flag, and a `demo` ceiling is refused for the tier
/// that does not exist.
#[test]
fn an_empty_store_names_the_exec_flag_and_a_demo_ceiling_names_the_missing_tier() {
    let default_row = |mode: VenueMode| -> VenueArming {
        vike_mount::venue_account_arming(REGISTRY, VENUE, &HashMap::new(), Some(&armed(mode)))
            .into_iter()
            .find(VenueArming::is_default_account)
            .expect("the default account's row")
    };
    assert_eq!(
        default_row(VenueMode::Live).block,
        ArmingBlock::ExecFlagUnset,
        "with the feature ON, an empty map's first missing conjunct is POLY_EXEC"
    );
    let demo = default_row(VenueMode::Demo);
    assert_eq!(
        (demo.effective, demo.block),
        (VenueMode::Paper, ArmingBlock::LiveOnlyArm),
        "polymarket runs no testnet, so a demo ceiling leaves it on paper"
    );
}

/// MOVED from `vike-mount`'s `tests/account_credential_isolation.rs`, its polymarket case, over
/// the REAL projection: the fixture arms the default account; a labelled account reads none of the
/// default account's keys; a labelled key set arms the labelled account and only it; a second
/// account's key perturbs nothing about the first account's row; and an unconfigured labelled
/// account reads like an unconfigured default one. (Its half-written row skips a single-key venue,
/// and this is one.) The exec flag is DEPLOYMENT-wide — it names no wallet — so every store carries
/// it unlabelled.
#[test]
fn a_labelled_polymarket_account_reads_only_its_own_key() {
    const KEY: &str = "POLY_LIVE_PRIVATE_KEY";
    let policy = MountPolicy {
        venues: VenuePolicy::default().declare(VENUE, VenueMode::Live).declare_account(
            VENUE,
            &alt(),
            VenueMode::Live,
        ),
        ..Default::default()
    };
    let row = |label: &AccountLabel, store: &HashMap<String, String>| -> VenueArming {
        vike_mount::venue_account_arming(REGISTRY, VENUE, store, Some(&policy))
            .into_iter()
            .find(|r| r.label == *label)
            .unwrap_or_else(|| panic!("polymarket: no row for {label}"))
    };
    let store = |label: &AccountLabel| -> HashMap<String, String> {
        let mut m = vars(&[("POLY_EXEC", "1")]);
        m.insert(account_key(KEY, label), "0xkey".to_string());
        m
    };
    let default = AccountLabel::Default;

    let default_keys = store(&default);
    let armed_default = row(&default, &default_keys);
    assert_eq!(
        armed_default.effective,
        VenueMode::Live,
        "polymarket: the fixture key set must ARM the default account, or this test proves \
         nothing ({:?})",
        armed_default.block
    );
    let leaked = row(&alt(), &default_keys);
    assert_eq!(
        (leaked.effective, leaked.block),
        (VenueMode::Paper, ArmingBlock::NoCredentials),
        "polymarket armed a LABELLED account off the DEFAULT account's keys — a second live client \
         signing for the first account"
    );

    let alt_keys = store(&alt());
    assert_eq!(
        row(&alt(), &alt_keys).effective,
        VenueMode::Live,
        "polymarket: labelled account NOT armed by its own keys"
    );
    let lost = row(&default, &alt_keys);
    assert_eq!(
        (lost.effective, lost.block),
        (VenueMode::Paper, ArmingBlock::NoCredentials),
        "polymarket: the DEFAULT account armed off a LABELLED key set"
    );

    let mut beside = default_keys.clone();
    beside.insert(account_key(KEY, &alt()), "0xkey-second-account".to_string());
    assert_eq!(
        row(&default, &default_keys),
        row(&default, &beside),
        "a second account's credentials changed the FIRST account's row"
    );

    let empty = vars(&[("POLY_EXEC", "1")]);
    assert_eq!(
        row(&alt(), &empty).block,
        row(&default, &empty).block,
        "polymarket: an unconfigured labelled account must read like an unconfigured default one"
    );
}

/// MOVED from `vike-mount`'s `tests/book_identity_table.rs`: its polymarket tuples in
/// `every_named_row_resolves_from_its_own_keys` (the `Named` row resolves from the REAL key name,
/// through the real label grammar) and `polymarket_has_no_demo_tier_to_read` (a demo-resolved
/// account reads no key at all, which matches the mount's `LiveOnlyArm` below `live`).
#[test]
fn the_polymarket_book_is_named_by_its_own_address_and_no_demo_tier_exists() {
    let default = AccountLabel::Default;
    // The live tier — its one spelling: `MAINNET` is no tier (owner ruling 2026-10-09).
    let key = "POLY_LIVE_ADDRESS";
    let v = vars(&[(key, "0xFunder")]);
    assert_eq!(
        effective_book(REGISTRY, VENUE, &default, VenueMode::Live, &v).as_deref(),
        Some("0xfunder"),
        "{VENUE}: {key} must name the book"
    );
    // …and the LABELLED account reads its OWN key, never the default account's — the same
    // no-fallback rule every credential loader in this workspace obeys.
    assert_eq!(
        effective_book(REGISTRY, VENUE, &alt(), VenueMode::Live, &v),
        None,
        "{VENUE}: a labelled account must not fall back to the default account's {key}"
    );
    let labelled = vars(&[(&format!("{key}__ALT"), "0xFunder")]);
    assert_eq!(
        effective_book(REGISTRY, VENUE, &alt(), VenueMode::Live, &labelled).as_deref(),
        Some("0xfunder"),
        "{VENUE}: {key}__ALT is the labelled account's own key"
    );

    let v = vars(&[("POLY_LIVE_ADDRESS", "0xfunder")]);
    assert_eq!(effective_book(REGISTRY, VENUE, &default, VenueMode::Demo, &v), None);
    assert_eq!(
        effective_book(REGISTRY, VENUE, &default, VenueMode::Live, &v).as_deref(),
        Some("0xfunder")
    );
}

/// The roster's one ORDER-affecting clock gap, answered through the real row: the assertion
/// `vike-mount`'s `a_declared_venue_with_orders_at_stake_reports_the_risk_not_a_shrug` made over
/// this row before its own registry carried the venue `FeatureAbsent`. Pure — no network.
#[test]
fn polymarket_answers_the_at_risk_clock_outcome_through_the_registry() {
    let Some(vike_bridge_core::venue_mount::ClockDecl::NotWired {
        reason,
        unmeasured_risk: Some(at_stake),
    }) = vike_mount::server_time::clock_decl(REGISTRY, VENUE)
    else {
        panic!("polymarket's row declares its clock and the risk it leaves unmeasured");
    };
    let gap =
        vike_mount::server_time::venue_server_time_ms(REGISTRY, VENUE, &HashMap::new(), false)
            .expect_err("declared venues never measure");
    assert_eq!(gap, ServerTimeGap::UnmeasuredRisk { reason, at_stake });
}

/// Under the feature the row is the BRIDGE's mount — a `cfg` pair typed the wrong way round would
/// leave the daemon mounting polymarket paper with no error.
#[test]
fn under_the_feature_the_registry_row_is_the_bridges_mount() {
    match vike_mount::row_of(REGISTRY, VENUE) {
        Some(VenueRow::Mount(m)) => assert_eq!(m.venue(), VENUE),
        other => panic!("polymarket's row under the `polymarket` feature is {other:?}"),
    }
}
