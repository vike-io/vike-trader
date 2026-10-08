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

#[path = "support/permissive_grid.rs"]
mod permissive_grid;

use std::collections::{HashMap, HashSet};

use vike_config::{ArmingBlock, VenueMode, VenuePolicy};
use vike_mount::MountPolicy;
use vike_tradehub::registry::REGISTRY;

use permissive_grid::assert_permissive_grid;

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
/// `the_pure_decision_names_a_stand_alone_live_login_then_the_shim_then_the_demo_login`.)
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
    let creds = fxcm_creds();
    let mut live = HashSet::new();
    let mut env = vike_mount::MountEnv::new(REGISTRY, &creds, &tx, &mut live);
    env.recon_enabled = true;
    env.policy = Some(&policy);
    let (engine, recon) = vike_mount::make_engine(&mut env, "fxcm", "EURUSD")
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
    assert_permissive_grid("fxcm", &engine.gate.limits);

    // The CONTROL: the same process with the credentials REMOVED reaches the same paper outcome
    // by the ordinary unconfigured path, so the assertions above pin the refusal and not some
    // unrelated breakage that would make every fxcm mount paper.
    let mut live2 = HashSet::new();
    let no_creds = HashMap::new();
    let mut env = vike_mount::MountEnv::new(REGISTRY, &no_creds, &tx, &mut live2);
    env.recon_enabled = true;
    env.policy = Some(&policy);
    let (_e, recon2) = vike_mount::make_engine(&mut env, "fxcm", "EURUSD")
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

/// A key state of the two-tier store, in the order the matrix walks them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Keys {
    None,
    DemoOnly,
    LiveOnly,
    Both,
}

const STATES: [Keys; 4] = [Keys::None, Keys::DemoOnly, Keys::LiveOnly, Keys::Both];

fn fxcm_store(state: Keys) -> HashMap<String, String> {
    let demo = [("FXCM_DEMO_USER", "D251112911"), ("FXCM_DEMO_PASSWORD", "not-a-real-password")];
    let live = [("FXCM_LIVE_USER", "L251112911"), ("FXCM_LIVE_PASSWORD", "not-a-real-password")];
    let (d, l) = match state {
        Keys::None => (false, false),
        Keys::DemoOnly => (true, false),
        Keys::LiveOnly => (false, true),
        Keys::Both => (true, true),
    };
    demo.iter()
        .filter(|_| d)
        .chain(live.iter().filter(|_| l))
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

/// **THE ONE RULE** the arming screen and the log both follow, as a function of the shim and the
/// store: a stand-alone LIVE login is named first (a fact about the STORE, which no box can change),
/// then the shim (a fact about the BOX), then the demo login. `crates/bridges/fxcm/src/mount.rs`'s
/// `resolution_for` is the code; this is its table, written out cell by cell so a change to the code
/// is a change to a line here. The expectation takes `sdk` as a parameter rather than reading
/// `sdk_available()` so the table is the same on a box that has the shim and on every CI runner.
fn fxcm_cell(sdk: bool, state: Keys, ceiling: VenueMode) -> (VenueMode, ArmingBlock) {
    match state {
        // A live login the arm will not use: named, not called missing — on every box.
        Keys::LiveOnly => (VenueMode::Paper, ArmingBlock::LiveTierNotWired),
        _ if !sdk => (VenueMode::Paper, ArmingBlock::SdkAbsent),
        Keys::DemoOnly | Keys::Both => {
            let held = if ceiling == VenueMode::Live {
                ArmingBlock::DemoOnlyArm
            } else {
                ArmingBlock::None
            };
            (VenueMode::Demo, held)
        }
        Keys::None => (VenueMode::Paper, ArmingBlock::NoCredentials),
    }
}

/// **FXCM's stay-paper matrix: every ceiling x every key state, pinned cell by cell** — the
/// feature-on twin of `mount_roster/stay_paper_matrix.rs`, which can only see fxcm as a
/// `FeatureAbsent` row. The rule is [`fxcm_cell`]'s; the SHIM answer is the box's own
/// (`sdk_available()`), so the projection is checked against the table on a box that has the shim
/// and on every CI runner, which has none.
///
/// Offline: the arming projection is the pin (a mounted fxcm cell needs a loadable shim).
#[test]
fn the_fxcm_stay_paper_matrix() {
    let sdk = vike_fxcm::sdk_available();
    for state in STATES {
        for ceiling in [VenueMode::Demo, VenueMode::Live] {
            assert_eq!(
                vike_mount::venue_arming_under(REGISTRY, "fxcm", &fxcm_store(state), ceiling),
                fxcm_cell(sdk, state, ceiling),
                "fxcm: keys={state:?} under a `{ceiling}` ceiling (shim loaded: {sdk})"
            );
        }
    }
    assert_eq!(
        vike_mount::venue_arming_under(REGISTRY, "fxcm", &fxcm_creds(), VenueMode::Paper),
        (VenueMode::Paper, ArmingBlock::Disarmed)
    );
}

/// The cells whose BLOCK this change renamed, as `(ceiling, key state, shim loaded, the block `main`
/// printed there)` — every other cell of the matrix equals `main`'s, block included.
///
/// Exactly one state moves: a stand-alone LIVE login on a box WITHOUT the shim, which printed
/// `SdkAbsent` on the screen while the log said `LiveTierNotWired`. With the shim loaded that cell
/// was already `LiveTierNotWired`, and no cell moves — which is why the list is empty on such a box.
/// No cell changes its effective mode: a cause moved, a tier did not.
/// [`the_fxcm_renamed_cells_are_exactly_the_ones_that_changed`] holds the list against the table in
/// both directions.
const RENAMED: &[(VenueMode, Keys, bool, ArmingBlock)] = &[
    (VenueMode::Demo, Keys::LiveOnly, false, ArmingBlock::SdkAbsent),
    (VenueMode::Live, Keys::LiveOnly, false, ArmingBlock::SdkAbsent),
];

/// [`RENAMED`] is exactly the set of cells whose block `main` printed differently, and each names a
/// cell that is still paper — a rename of a cause that also changed the tier would not be a rename.
#[test]
fn the_fxcm_renamed_cells_are_exactly_the_ones_that_changed() {
    for (ceiling, state, sdk, before) in RENAMED {
        let (effective, block) = fxcm_cell(*sdk, *state, *ceiling);
        assert_eq!(effective, VenueMode::Paper, "{ceiling} {state:?}: a renamed cause stays paper");
        assert_ne!(block, *before, "{ceiling} {state:?}: listed as renamed, but unchanged");
        assert_eq!(block, ArmingBlock::LiveTierNotWired, "{ceiling} {state:?}: the new cause");
        assert_eq!(*before, ArmingBlock::SdkAbsent, "{ceiling} {state:?}: what `main` printed");
        assert!(!*sdk, "the rename exists only where the shim is absent");
    }
    // …and the other direction: every shim-less cell that now prints `LiveTierNotWired` is listed,
    // so a cell cannot adopt the cause without a line here that says what it used to print. With
    // the shim loaded the cause is not new at all.
    for state in STATES {
        for ceiling in [VenueMode::Demo, VenueMode::Live] {
            let (_, block) = fxcm_cell(false, state, ceiling);
            if block == ArmingBlock::LiveTierNotWired {
                assert!(
                    RENAMED.iter().any(|(c, s, sdk, _)| *c == ceiling && *s == state && !*sdk),
                    "{ceiling} {state:?}: prints the cause without the shim but is not listed in RENAMED"
                );
            }
        }
    }
}
