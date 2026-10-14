//! FXCM through the REAL registry — the tests that drive `vike-tradehub-core`'s PUBLIC fold with the real
//! venue id. They ran in `vike-tradehub-core` under its own `fxcm` feature until the venue-adapter contract
//! moved that feature, and the bridge dependency with it, to this crate, which holds the registry
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md, amended 2026-09-29).
//! Their assertions are the ones they had there, except that the refusal's reconcile half now says
//! what it can prove.
//!
//! Compiled only under this crate's `fxcm` feature (`ci.yml`'s `tradehub-all` job, `just tradehub-all`);
//! without it the registry carries fxcm `FeatureAbsent`. That crate-level `#![cfg]` is why this
//! is its own test binary rather than a `daemon` member. No CI runner stages the ForexConnect
//! SDK, so every lane takes the REFUSING branch; each test says what it asserts on a box that
//! has the shim. The bridge's own gates — the pure shim-then-login decision, the refusal and the
//! order of the start's steps, the LIVE branch, the declaration — are
//! `crates/bridges/fxcm/src/adapter_tests.rs`'s.
#![cfg(feature = "fxcm")]

use std::collections::{HashMap, HashSet};

use vike_config::ArmingBlock;
use vike_model::AccountTier;
use vike_secrets::account_credentials::{AccountCredentials, CredentialFields};
use vike_tradehub::registry::REGISTRY;
use vike_tradehub_core::AdapterPolicy;

/// One active DEFAULT-account `fxcm` row at `mode`.
fn armed(mode: AccountTier) -> AdapterPolicy {
    AdapterPolicy::default().with_account(
        "fxcm",
        &vike_model::accounts::account_keys::AccountLabel::Default,
        mode,
    )
}

/// The login an fxcm account holds, by FIELD (no tier rides on a credential).
const LOGIN: &[(&str, &str)] = &[("USER", "D251112911"), ("PASSWORD", "not-a-real-password")];

fn fxcm_creds() -> CredentialFields {
    CredentialFields::from_pairs(LOGIN.iter().copied())
}

/// One active DEFAULT-account `fxcm` row at `mode`, holding its login.
fn armed_holding_login(mode: AccountTier) -> AdapterPolicy {
    AdapterPolicy::default().with_credentials(
        "fxcm",
        &vike_model::accounts::account_keys::AccountLabel::Default,
        mode,
        LOGIN,
    )
}

/// MOVED from `vike-tradehub-core`'s fee-schedule tests: credentials PRESENT, shim NOT loadable ⇒ the
/// start REFUSES and the account does NOT RUN — its exec client could place nothing — with nothing
/// reconciling and no paper engine in its place (decision 0156). SELF-SKIPS on a box that loads the
/// shim, where starting live is correct. (The pure probe half it used to assert last is
/// `crates/bridges/fxcm/src/adapter_tests.rs`'s
/// `the_pure_decision_names_a_stand_alone_live_login_then_the_shim_then_the_demo_login`.)
#[test]
fn fxcm_credentials_without_a_loadable_shim_refuse_the_live_start() {
    if vike_fxcm::sdk_available() {
        eprintln!("skipping: this process loaded the ForexConnect shim, so fxcm mounts live here");
        return;
    }
    let (tx, _rx) = vike_exec::event_channel(16);
    // `recon_enabled: true` and an active `demo` row for fxcm: the refusal is what is pinned, not
    // the global reconcile gate or the paper early return.
    let policy = armed_holding_login(AccountTier::Demo);
    let creds = HashMap::new();
    let mut live = HashSet::new();
    let mut env = vike_tradehub_core::AdapterEnv::new(REGISTRY, &creds, &tx, &mut live);
    env.recon_enabled = true;
    env.policy = Some(&policy);
    let stopped = not_running(vike_tradehub_core::make_engine(&mut env, "fxcm", "EURUSD"));
    assert_eq!(
        (stopped.tier, stopped.block),
        (Some(AccountTier::Demo), ArmingBlock::SdkAbsent),
        "no shim: the account does NOT RUN, naming the shim — never a paper engine in its place \
         (decision 0156)"
    );
    assert!(
        live.is_empty(),
        "no shim must NOT mark fxcm live: its exec client could place nothing"
    );

    // The CONTROL: the same process with the login REMOVED does not run either, by the ordinary
    // unconfigured path, so the assertions above pin the refusal and not some unrelated breakage
    // that would stop every fxcm start.
    let mut live2 = HashSet::new();
    let no_creds = HashMap::new();
    let unconfigured = armed(AccountTier::Demo);
    let mut env = vike_tradehub_core::AdapterEnv::new(REGISTRY, &no_creds, &tx, &mut live2);
    env.recon_enabled = true;
    env.policy = Some(&unconfigured);
    let stopped = not_running(vike_tradehub_core::make_engine(&mut env, "fxcm", "EURUSD"));
    assert!(!stopped.block.runs(), "{:?}", stopped.block);
    assert!(live2.is_empty());
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

/// MOVED from `vike-tradehub-core`'s `fxcm_probes_live_only_with_credentials_and_a_linked_sdk` — its WIRED
/// half (the pure half is the bridge's). With credentials present the probe IS the shim question,
/// asserted against `sdk_available()` rather than a literal so it is honest on both kinds of box.
/// Probed for the `demo` account its login arms; a `live` account holding the login is paper
/// (`LiveTierNotWired`, decision 0119), so that probe is `false` on every box.
#[test]
fn with_credentials_present_the_probe_is_exactly_the_shim_question() {
    let held = fxcm_creds();
    let creds = AccountCredentials::new(Some(1), Some(&held), None);
    assert_eq!(
        vike_tradehub_core::would_go_live_under(
            REGISTRY,
            "fxcm",
            &HashMap::new(),
            creds,
            AccountTier::Demo
        ),
        vike_fxcm::sdk_available(),
        "with credentials present the probe is exactly the SDK question"
    );
    assert!(
        !vike_tradehub_core::would_go_live_under(
            REGISTRY,
            "fxcm",
            &HashMap::new(),
            creds,
            AccountTier::Live
        ),
        "a `live` account holding a login the arm cannot trade is paper, never demo"
    );
}

/// MOVED from `vike-tradehub-core`'s `a_venue_this_build_cannot_start_says_so_instead_of_blaming_credentials`
/// (its feature-on half): an empty store under a `live` account is refused for the SHIM first.
#[test]
fn an_empty_store_is_refused_for_the_shim_before_the_credentials() {
    let rows = vike_tradehub_core::venue_account_arming(
        REGISTRY,
        "fxcm",
        &HashMap::new(),
        Some(&armed(AccountTier::Live)),
    );
    let row = rows.iter().find(|r| r.is_default_account()).expect("the default account's row");
    let block = if vike_fxcm::sdk_available() {
        ArmingBlock::NoCredentials
    } else {
        ArmingBlock::SdkAbsent
    };
    assert_eq!((row.effective, row.block), (None, block));
}

/// What the account holds, in the order the matrix walks it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Keys {
    None,
    Held,
}

const STATES: [Keys; 2] = [Keys::None, Keys::Held];

fn fxcm_fields(state: Keys) -> CredentialFields {
    match state {
        Keys::None => CredentialFields::new(),
        Keys::Held => fxcm_creds(),
    }
}

/// **THE ONE RULE** the arming projection and the log both follow, as a function of the shim and the
/// account: a `live` account's complete login is named first (a fact about the ACCOUNT, which no box
/// can change), then the shim (a fact about the BOX), then the demo login.
/// `crates/bridges/fxcm/src/adapter.rs`'s `resolution_for` is the code; this is its table, written
/// out cell by cell so a change to the code is a change to a line here. A `live` account holding the
/// login is PAPER with `LiveTierNotWired`, never demo (decision 0119's no-downgrade rule). The
/// expectation takes `sdk` as a parameter rather than reading `sdk_available()` so the table is the
/// same on a box that has the shim and on every CI runner.
fn fxcm_cell(sdk: bool, state: Keys, tier: AccountTier) -> (Option<AccountTier>, ArmingBlock) {
    match (state, tier) {
        // A live login the arm will not use: named, not called missing — on every box.
        (Keys::Held, AccountTier::Live) => (None, ArmingBlock::LiveTierNotWired),
        _ if !sdk => (None, ArmingBlock::SdkAbsent),
        (Keys::Held, _) => (Some(AccountTier::Demo), ArmingBlock::None),
        (Keys::None, _) => (None, ArmingBlock::NoCredentials),
    }
}

/// **FXCM's stay-paper matrix: every account tier x every credential state, pinned cell by cell** — the
/// feature-on twin of `mount_roster/stay_paper_matrix.rs`, which can only see fxcm as a
/// `FeatureAbsent` row. The rule is [`fxcm_cell`]'s; the SHIM answer is the box's own
/// (`sdk_available()`), so the projection is checked against the table on a box that has the shim
/// and on every CI runner, which has none.
///
/// Offline: the arming projection is the pin (a started fxcm cell needs a loadable shim).
#[test]
fn the_fxcm_stay_paper_matrix() {
    let sdk = vike_fxcm::sdk_available();
    for state in STATES {
        for tier in [AccountTier::Demo, AccountTier::Live] {
            let held = fxcm_fields(state);
            assert_eq!(
                vike_tradehub_core::venue_arming_under(
                    REGISTRY,
                    "fxcm",
                    &HashMap::new(),
                    AccountCredentials::new(Some(1), Some(&held), None),
                    tier
                ),
                fxcm_cell(sdk, state, tier),
                "fxcm: keys={state:?} for a `{tier}` account (shim loaded: {sdk})"
            );
        }
    }
    let held = fxcm_creds();
    assert_eq!(
        vike_tradehub_core::venue_arming_under(
            REGISTRY,
            "fxcm",
            &HashMap::new(),
            AccountCredentials::new(Some(1), Some(&held), None),
            AccountTier::Paper
        ),
        (Some(AccountTier::Paper), ArmingBlock::PaperTier)
    );
}

/// The cells whose BLOCK the `LiveTierNotWired` change renamed, as `(tier, key state, shim loaded, the block `main`
/// printed there)` — every other cell of the matrix equals `main`'s, block included.
///
/// Exactly one cell moves: a `live` account's complete login on a box WITHOUT the shim, which printed
/// `SdkAbsent` on the screen while the log said `LiveTierNotWired`. With the shim loaded that cell
/// was already `LiveTierNotWired`, and no cell moves — which is why the list is empty on such a box.
/// No cell changes its effective mode: a cause moved, a tier did not.
/// [`the_fxcm_renamed_cells_are_exactly_the_ones_that_changed`] holds the list against the table in
/// both directions.
const RENAMED: &[(AccountTier, Keys, bool, ArmingBlock)] =
    &[(AccountTier::Live, Keys::Held, false, ArmingBlock::SdkAbsent)];

/// [`RENAMED`] is exactly the set of cells whose block `main` printed differently, and each names a
/// cell that is still paper — a rename of a cause that also changed the tier would not be a rename.
#[test]
fn the_fxcm_renamed_cells_are_exactly_the_ones_that_changed() {
    for (tier, state, sdk, before) in RENAMED {
        let (effective, block) = fxcm_cell(*sdk, *state, *tier);
        assert_eq!(effective, None, "{tier} {state:?}: a renamed cause still does not run");
        assert_ne!(block, *before, "{tier} {state:?}: listed as renamed, but unchanged");
        assert_eq!(block, ArmingBlock::LiveTierNotWired, "{tier} {state:?}: the new cause");
        assert_eq!(*before, ArmingBlock::SdkAbsent, "{tier} {state:?}: what `main` printed");
        assert!(!*sdk, "the rename exists only where the shim is absent");
    }
    // …and the other direction: every shim-less cell that now prints `LiveTierNotWired` is listed,
    // so a cell cannot adopt the cause without a line here that says what it used to print. With
    // the shim loaded the cause is not new at all.
    for state in STATES {
        for tier in [AccountTier::Demo, AccountTier::Live] {
            let (_, block) = fxcm_cell(false, state, tier);
            if block == ArmingBlock::LiveTierNotWired {
                assert!(
                    RENAMED.iter().any(|(c, s, sdk, _)| *c == tier && *s == state && !*sdk),
                    "{tier} {state:?}: prints the cause without the shim but is not listed in RENAMED"
                );
            }
        }
    }
}
