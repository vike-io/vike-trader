//! FXCM through the REAL registry — the tests that drive `vike-mount`'s PUBLIC fold with the real
//! venue id. They ran in `vike-mount` under its own `fxcm` feature until the venue mount contract
//! moved that feature, and the bridge dependency with it, to this crate, which holds the registry
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md, amended 2026-09-29).
//! Their assertions are the ones they had there, except that the refusal's reconcile half now says
//! what it can prove.
//!
//! Compiled only under this crate's `fxcm` feature (`bash scripts/ci_feature_suite.sh fxcm`);
//! without it the registry carries fxcm `FeatureAbsent`. That crate-level `#![cfg]` is why this
//! is its own test binary rather than a `daemon` member. No CI runner stages the ForexConnect
//! SDK, so every lane takes the REFUSING branch; each test says what it asserts on a box that
//! has the shim. The bridge's own gates — the pure shim-then-login decision, the refusal and the
//! order of the mount's steps, the LIVE branch, the declaration — are
//! `crates/bridges/fxcm/src/mount_tests.rs`'s.
#![cfg(feature = "fxcm")]

use std::collections::{HashMap, HashSet};

use vike_config::{ArmingBlock, VenueMode, VenuePolicy};
use vike_mount::MountPolicy;
use vike_tradehub::registry::REGISTRY;

fn armed(mode: VenueMode) -> MountPolicy {
    MountPolicy { venues: VenuePolicy::default().declare("fxcm", mode), ..Default::default() }
}

fn fxcm_creds() -> HashMap<String, String> {
    [("FXCM_DEMO_USER", "D251112911"), ("FXCM_DEMO_PASSWORD", "not-a-real-password")]
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

/// MOVED from `vike-mount`'s fee-schedule tests: credentials PRESENT, shim NOT loadable ⇒ the
/// mount REFUSES and lands on paper — its exec client could place nothing — with the paper fee
/// schedule and the permissive grid, and nothing reconciling. SELF-SKIPS on a box that loads the
/// shim, where mounting live is correct. (The pure probe half it used to assert last is
/// `crates/bridges/fxcm/src/mount_tests.rs`'s
/// `the_pure_decision_asks_the_shim_first_then_the_credentials`.)
#[test]
fn fxcm_credentials_without_a_loadable_shim_refuse_the_live_mount() {
    if vike_fxcm::sdk_available() {
        eprintln!("skipping: this process loaded the ForexConnect shim, so fxcm mounts live here");
        return;
    }
    let (tx, _rx) = vike_exec::event_channel(16);
    // `recon_enabled: true` and a ceiling that PERMITS fxcm: the refusal is what is pinned, not
    // the global reconcile gate or the paper early return.
    let policy = armed(VenueMode::Demo);
    let mut live = HashSet::new();
    let (engine, recon) = vike_mount::make_engine(
        REGISTRY,
        "fxcm",
        "EURUSD",
        &fxcm_creds(),
        &tx,
        &mut live,
        true,
        None,
        None,
        None,
        Some(&policy),
    )
    .expect("a paper mount must never refuse to start");
    assert!(
        live.is_empty(),
        "no shim must NOT mark fxcm live: its exec client could place nothing"
    );
    assert!(
        recon.is_none(),
        "nothing reconciles. On this box a reconcile login would fail without the shim too, so this \
         cannot see the ORDER of the steps; the bridge's \
         `the_refusal_comes_before_the_reconcile_factory` pins that"
    );
    assert_eq!(engine.fee_schedule, Some(vike_model::fee_schedule_for("fxcm")));
    let limits = &engine.gate.limits;
    assert_eq!(limits.tick_size, None, "fxcm: permissive grid has no tick constraint");
    assert_eq!(limits.lot_size, None, "fxcm: permissive grid has no lot constraint");
    assert_eq!(limits.min_qty, None, "fxcm: permissive grid has no min-qty constraint");
    assert_eq!(limits.min_notional, None, "fxcm: permissive grid has no notional constraint");
    assert!(
        limits.grid_by_symbol.is_empty(),
        "fxcm: a mount with no declared legs must carry no per-symbol grid"
    );

    // The CONTROL: the same process with the credentials REMOVED reaches the same paper outcome
    // by the ordinary unconfigured path, so the assertions above pin the refusal and not some
    // unrelated breakage that would make every fxcm mount paper.
    let mut live2 = HashSet::new();
    let (_e, recon2) = vike_mount::make_engine(
        REGISTRY,
        "fxcm",
        "EURUSD",
        &HashMap::new(),
        &tx,
        &mut live2,
        true,
        None,
        None,
        None,
        Some(&policy),
    )
    .expect("a paper mount must never refuse to start");
    assert!(live2.is_empty() && recon2.is_none());
}

/// MOVED from `vike-mount`'s `fxcm_probes_live_only_with_credentials_and_a_linked_sdk` — its WIRED
/// half (the pure half is the bridge's). With credentials present the probe IS the shim question,
/// asserted against `sdk_available()` rather than a literal so it is honest on both kinds of box.
#[test]
fn with_credentials_present_the_probe_is_exactly_the_shim_question() {
    assert_eq!(
        vike_mount::would_mount_live(REGISTRY, "fxcm", &fxcm_creds()),
        vike_fxcm::sdk_available(),
        "with credentials present the probe is exactly the SDK question"
    );
}

/// MOVED from `vike-mount`'s `a_venue_this_build_cannot_mount_says_so_instead_of_blaming_credentials`
/// (its feature-on half): an empty store under a `live` ceiling is refused for the SHIM first.
#[test]
fn an_empty_store_is_refused_for_the_shim_before_the_credentials() {
    let rows = vike_mount::venue_account_arming(
        REGISTRY,
        "fxcm",
        &HashMap::new(),
        Some(&armed(VenueMode::Live)),
    );
    let row = rows.iter().find(|r| r.is_default_account()).expect("the default account's row");
    let block = if vike_fxcm::sdk_available() {
        ArmingBlock::NoCredentials
    } else {
        ArmingBlock::SdkAbsent
    };
    assert_eq!((row.effective, row.block), (VenueMode::Paper, block));
}
