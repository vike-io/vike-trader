//! IBKR through the REAL registry — the tests that drive `vike-mount`'s PUBLIC fold with the real
//! venue id. They ran in `vike-mount` until the venue mount contract moved its `ibkr` feature, and
//! the bridge dependency with it, to this crate
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md, amended 2026-09-29):
//! `vike-mount`'s own registry can no longer name the bridge, and a bridge cannot call `vike-mount`
//! without a dependency cycle, so this crate — which holds the registry and sees both — is where
//! they run. Their assertions are the ones they had there.
//!
//! Compiled only under this crate's `ibkr` feature (`bash scripts/ci_feature_suite.sh ibkr`);
//! without it the registry carries ibkr `FeatureAbsent` and there is no bridge to reach. That
//! crate-level `#![cfg]` is also why this is its own test binary rather than a `daemon` member.
//! The bridge's own gates — its loader, its demotion, its declaration — are
//! `crates/bridges/vike-ibkr/src/mount_tests.rs`'s.
#![cfg(feature = "ibkr")]

#[path = "support/permissive_grid.rs"]
mod permissive_grid;

use std::collections::{HashMap, HashSet};

use vike_config::{ArmingBlock, VenueArming, VenueMode};
use vike_model::accounts::account_keys::{AccountLabel, account_key};
use vike_mount::MountPolicy;
use vike_mount::book_identity::effective_book;
use vike_tradehub::registry::REGISTRY;

use permissive_grid::assert_permissive_grid;

fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// Whether anything accepts a TCP connection on `127.0.0.1:port` — the same probe the bridge's
/// dead-port test uses, for the same reason: the skip is decided BEFORE mounting, never from the
/// mount's own outcome, or the outcome the test asserts on could not fail.
fn something_listens_on(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        std::time::Duration::from_millis(250),
    )
    .is_ok()
}

/// MOVED from `vike-mount`'s fee-schedule tests. The connect-failure DEMOTION, end to end through
/// the fold: config PRESENT but pointed at dead ports (no TWS socket, no CP Gateway), so the
/// connect fails fast (connection refused) and the venue is NOT marked live — and the cpapi
/// reconcile client resolves `None` for the same reason. Needs NO running Gateway. SELF-SKIPS,
/// before mounting, if anything answers on the chosen ports: it used to skip on the mount's own
/// `live` set, which made the `live.is_empty()` assertion below impossible to fail.
#[test]
fn ibkr_present_config_without_gateway_demotes_to_paper() {
    if something_listens_on(65534) || something_listens_on(65533) {
        eprintln!("skipping: something answers on the IBKR test ports 65534/65533");
        return;
    }
    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = HashSet::new();
    // Dead ports: nothing listens on 127.0.0.1:6553{4,3}, so both the socket exec connect and the
    // cpapi recon tickle get an immediate connection-refused.
    let v = vars(&[("IBKR_DEMO_ACCOUNT", "DUTEST000")]);
    // The gateway is the DEMO tier's `venue.ibkr.demo.{backend,port,cpapi_url}` rows (decision
    // 0095), which reach the mount through `MountPolicy::venue_settings`, never the credential map.
    let gateway: Vec<vike_secrets::VenueSettingRow> =
        [("BACKEND", "socket"), ("PORT", "65534"), ("CPAPI_URL", "https://127.0.0.1:65533")]
            .into_iter()
            .map(|(field, value)| vike_secrets::VenueSettingRow {
                venue: "ibkr".to_string(),
                tier: Some("demo".to_string()),
                field: field.to_string(),
                value: value.to_string(),
            })
            .collect();
    // ⚠ A RISK BUDGET IS PART OF THIS FIXTURE, and fxcm's demotion twin
    // (`crates/vike-tradehub/tests/fxcm_mount.rs`'s
    // `fxcm_credentials_without_a_loadable_shim_refuse_the_live_mount`) deliberately has none.
    // The asymmetry is BY CONSTRUCTION — do not "harmonize" the two by deleting this.
    //
    // `make_engine_for_account` runs a PRE-CONNECT refusal (the #817 "the refusal happens
    // POST-connect" fix, Freqtrade's shape): when `account_arming_under` says this mount is
    // live-INTENT, a missing `max_notional_per_order`/`max_total_exposure` is
    // `MountError::MissingRiskBudget` BEFORE anything is dialled. That probe is INTENT-based:
    // present live config is the operator's declared intent, even where the mount would later
    // demote to paper. ibkr's `resolve` reads `load_ibkr_config_for_account(Demo, …)`, which the
    // vars above satisfy, and the `demo` account row below arms it — so ibkr IS live-intent here, and with no
    // budget the mount refuses without ever reaching the connect-failure demotion this test exists
    // to observe. (Measured: on this test's first ever execution, once the `ibkr` CI lane widened
    // to compile it, that is exactly how it failed.)
    //
    // fxcm's row answers `(Paper, SdkAbsent)` on its FIRST conjunct — `sdk_available()` is false
    // on every CI runner — so its twin is not live-intent and never reaches the budget gate.
    // ibkr has no such conjunct available: its Gateway's liveness is knowable ONLY by attempting a
    // connect, which is precisely the I/O this gate exists to precede.
    //
    // The two caps below are the exact pair `require_live_risk_budget` reads, and nothing else is
    // set — a profile carrying no venue-owned field arms the operator budget and leaves the venue
    // GRID alone, so `assert_permissive_grid` below still asserts what it always did.
    let budget = vike_model::ProfileRisk {
        max_notional_per_order: Some(100.0),
        max_total_exposure: Some(500.0),
        ..Default::default()
    };
    // ⚠ ibkr's ACCOUNT must be armed (an active `demo` row), or the bridge is never reached at
    // all and this test passes against the paper early return — every assertion here is also true
    // of a paper-held mount, so without this line it would be vacuous rather than red.
    let policy = MountPolicy {
        venue_settings: std::collections::BTreeMap::from([(
            "ibkr".to_string(),
            vike_secrets::venue_setting::VenueSettings::from_rows("ibkr", &gateway),
        )]),
        ..MountPolicy::default().with_account("ibkr", &AccountLabel::Default, VenueMode::Demo)
    };
    let mut env = vike_mount::MountEnv::new(REGISTRY, &v, &tx, &mut live);
    // `recon_enabled: true` on purpose — the "no Gateway → recon unwired" assertion below is about
    // the cpapi handshake FAILING on a dead port, so the factory must actually be reached; `false`
    // would satisfy it through the global gate and pin nothing.
    env.recon_enabled = true;
    env.risk_profile = Some(&budget);
    env.policy = Some(&policy);
    let (engine, recon) = vike_mount::make_engine(&mut env, "ibkr", "AAPL.SMART.USD")
        .expect("budget supplied + no reachable Gateway -> the mount demotes to paper, not Err");
    assert!(recon.is_none(), "no Gateway → recon unwired");
    assert!(live.is_empty(), "connect failure → not marked live (demoted to paper)");
    assert_eq!(engine.fee_schedule, Some(vike_model::fee_schedule_for("ibkr")));
    // Demoted to paper: the `contractDetails` pre-fetch is never reached (it lives inside the
    // connected branch), so the limits stay permissive — same as every paper mount.
    assert_permissive_grid("ibkr", &engine.gate.limits);
}

/// MOVED from `vike-mount`'s `tests/account_credential_isolation.rs` (its ibkr case): the 2×2 over
/// the REAL projection — `venue_account_arming`, the one `make_engine_accounts` selects with —
/// plus the two equalities that file held every case to: a second account's key perturbs nothing
/// about the first account's row, and an unconfigured labelled account reads like an unconfigured
/// default one. (`_ACCOUNT` is ibkr's only key, so the half-written half of that file's last test
/// never applied to it.)
#[test]
fn a_labelled_ibkr_account_reads_only_its_own_keys() {
    const KEY: &str = "IBKR_DEMO_ACCOUNT";
    let policy = MountPolicy::default()
        .with_account("ibkr", &AccountLabel::Default, VenueMode::Demo)
        .with_account("ibkr", &alt(), VenueMode::Demo);
    let row = |label: &AccountLabel, store: &HashMap<String, String>| -> VenueArming {
        vike_mount::venue_account_arming(REGISTRY, "ibkr", store, Some(&policy))
            .into_iter()
            .find(|r| r.label == *label)
            .unwrap_or_else(|| panic!("ibkr: no row for {label}"))
    };
    let store = |label: &AccountLabel| -> HashMap<String, String> {
        [(account_key(KEY, label), "DU1234567".to_string())].into_iter().collect()
    };

    let default_keys = store(&AccountLabel::Default);
    assert_eq!(row(&AccountLabel::Default, &default_keys).effective, VenueMode::Demo);
    let leaked = row(&alt(), &default_keys);
    assert_eq!(
        (leaked.effective, leaked.block),
        (VenueMode::Paper, ArmingBlock::NoCredentials),
        "ALT armed off the DEFAULT account's `_ACCOUNT` — a second engine on the first book"
    );

    let alt_keys = store(&alt());
    assert_eq!(row(&alt(), &alt_keys).effective, VenueMode::Demo, "ALT's own key arms ALT");
    let lost = row(&AccountLabel::Default, &alt_keys);
    assert_eq!((lost.effective, lost.block), (VenueMode::Paper, ArmingBlock::NoCredentials));

    let mut beside = default_keys.clone();
    beside.insert(account_key(KEY, &alt()), "DU1234567-second-account".to_string());
    assert_eq!(
        row(&AccountLabel::Default, &default_keys),
        row(&AccountLabel::Default, &beside),
        "a second account's key must perturb nothing about the first account's row"
    );

    let empty = HashMap::new();
    assert_eq!(
        row(&alt(), &empty).block,
        row(&AccountLabel::Default, &empty).block,
        "an unconfigured labelled account must read like an unconfigured default one"
    );
}

/// MOVED from `vike-mount`'s `tests/book_identity_table.rs` (`every_named_row_resolves_from_its_own_keys`):
/// ibkr's `Named` row resolves from the REAL key names through the real label grammar. It left
/// that file because `vike-mount`'s own registry now answers ibkr's book with the generic
/// `FeatureAbsent` row. The LIVE-tier key is assembled with `concat!` rather than spelled as one
/// literal — a credential key NAME, not a path — like the bridge's own fixture.
#[test]
fn the_ibkr_book_is_named_by_its_own_account_key() {
    for (mode, key, value, expected) in [
        (VenueMode::Demo, "IBKR_DEMO_ACCOUNT", "DUQ186573", "duq186573"),
        (VenueMode::Live, concat!("IBKR", "_LIVE_ACCOUNT"), "U13112916", "u13112916"),
    ] {
        let v = vars(&[(key, value)]);
        assert_eq!(
            effective_book(REGISTRY, "ibkr", &AccountLabel::Default, mode, &v).as_deref(),
            Some(expected),
            "ibkr: {key} must name the book"
        );
        // …and the LABELLED account reads its OWN key, never the default account's — the same
        // no-fallback rule every credential loader in this workspace obeys.
        assert_eq!(
            effective_book(REGISTRY, "ibkr", &alt(), mode, &v),
            None,
            "ibkr: a labelled account must not fall back to the default account's {key}"
        );
        let labelled = vars(&[(&format!("{key}__ALT"), value)]);
        assert_eq!(
            effective_book(REGISTRY, "ibkr", &alt(), mode, &labelled).as_deref(),
            Some(expected),
            "ibkr: {key}__ALT is the labelled account's own key"
        );
    }
}

/// **IBKR's stay-paper matrix: every account tier x every key state, pinned cell by cell** — the
/// feature-on twin of `mount_roster/stay_paper_matrix.rs`, which can only see ibkr as a
/// `FeatureAbsent` row. The arm resolves the DEMO (paper-account) tier unless the LIVE tier arms,
/// and the LIVE tier arms only with an ACTIVE `account` row (and a `live` tier, a loadable
/// gateway and a socket backend) — this probe passes no account table, so nothing says the live
/// account is active and a LIVE-tier `_ACCOUNT` never arms here; what a store holding only that key
/// prints is the cause this test pins, and the effective mode beside it is the part that must not
/// move. A `live` account holding only the DEMO (paper-account) key is PAPER with
/// `LiveCredentialsAbsent`, never the held demo session (`DemoOnlyArm`) it once traded — a live
/// account never trades demo (decision 0119). (`crates/bridges/vike-ibkr/src/mount_tests.rs` holds
/// the arming cases.)
///
/// Offline: the arming projection is the pin. An armed cell would dial a Gateway, so it is not
/// mounted here (`ibkr_present_config_without_gateway_demotes_to_paper` is the mounted twin).
#[test]
fn the_ibkr_stay_paper_matrix() {
    let demo = [("IBKR_DEMO_ACCOUNT", "DU1234567")];
    let live = [("IBKR_LIVE_ACCOUNT", "U1234567")];
    let store = |d: bool, l: bool| -> HashMap<String, String> {
        let mut pairs: Vec<(&str, &str)> = Vec::new();
        if d {
            pairs.extend_from_slice(&demo);
        }
        if l {
            pairs.extend_from_slice(&live);
        }
        vars(&pairs)
    };
    // (demo keys, live keys, effective + block for a `demo` account, …for a `live` account)
    type Cell = (VenueMode, ArmingBlock);
    let none: Cell = (VenueMode::Paper, ArmingBlock::NoCredentials);
    // A LIVE-tier account the arm will not use: stays paper, and says which tier it found.
    let unwired: Cell = (VenueMode::Paper, ArmingBlock::LiveTierNotWired);
    // A `live` account the arm can honour only at DEMO: paper, and the live set is what is absent.
    let no_downgrade: Cell = (VenueMode::Paper, ArmingBlock::LiveCredentialsAbsent);
    let cases: [(bool, bool, Cell, Cell); 4] = [
        (false, false, none, none),
        (true, false, (VenueMode::Demo, ArmingBlock::None), no_downgrade),
        (false, true, unwired, unwired),
        (true, true, (VenueMode::Demo, ArmingBlock::None), no_downgrade),
    ];
    for (d, l, at_demo, at_live) in cases {
        for (tier, expected) in [(VenueMode::Demo, at_demo), (VenueMode::Live, at_live)] {
            assert_eq!(
                vike_mount::venue_arming_under(REGISTRY, "ibkr", &store(d, l), tier),
                expected,
                "ibkr: demo keys={d} live keys={l} for a `{tier}` account"
            );
        }
    }
    // …and a `paper` tier answers above the bridge whatever the store holds.
    assert_eq!(
        vike_mount::venue_arming_under(REGISTRY, "ibkr", &store(true, true), VenueMode::Paper),
        (VenueMode::Paper, ArmingBlock::PaperTier)
    );
}
