//! IBKR through the REAL registry — the tests that drive `vike-mount`'s PUBLIC fold with the real
//! venue id. They ran in `vike-mount` until the venue mount contract moved its `ibkr` feature, and
//! the bridge dependency with it, to this crate
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md, amended 2026-09-29):
//! `vike-mount`'s own registry can no longer name the bridge, and a bridge cannot call `vike-mount`
//! without a dependency cycle, so this crate — which holds the registry and sees both — is where
//! they run. Their assertions are the ones they had there.
//!
//! Compiled only under this crate's `ibkr` feature (`ci.yml`'s `tradehub-all` job, `just tradehub-all`);
//! without it the registry carries ibkr `FeatureAbsent` and there is no bridge to reach. That
//! crate-level `#![cfg]` is also why this is its own test binary rather than a `daemon` member.
//! The bridge's own gates — its loader, its demotion, its declaration — are
//! `crates/bridges/vike-ibkr/src/mount/tests.rs`'s.
#![cfg(feature = "ibkr")]

use std::collections::{HashMap, HashSet};

use vike_config::{ArmingBlock, VenueArming};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_mount::MountPolicy;
use vike_mount::book_identity::effective_book;
use vike_secrets::account_credentials::{AccountCredentials, CredentialFields};
use vike_tradehub::registry::REGISTRY;

fn fields(pairs: &[(&str, &str)]) -> CredentialFields {
    CredentialFields::from_pairs(pairs.iter().copied())
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

/// MOVED from `vike-mount`'s fee-schedule tests. The connect failure, end to end through the fold:
/// config PRESENT but pointed at dead ports (no TWS socket, no CP Gateway), so the connect fails
/// fast (connection refused), the account does NOT RUN (`MountFailed`, decision 0156 — never a
/// paper engine in its place), and the venue is NOT marked live — and the cpapi
/// reconcile client resolves `None` for the same reason. Needs NO running Gateway. SELF-SKIPS,
/// before mounting, if anything answers on the chosen ports: it used to skip on the mount's own
/// `live` set, which made the `live.is_empty()` assertion below impossible to fail.
#[test]
fn ibkr_present_config_without_gateway_does_not_run() {
    if something_listens_on(65534) || something_listens_on(65533) {
        eprintln!("skipping: something answers on the IBKR test ports 65534/65533");
        return;
    }
    let (tx, _rx) = vike_exec::event_channel(16);
    let mut live = HashSet::new();
    // Dead ports: nothing listens on 127.0.0.1:6553{4,3}, so both the socket exec connect and the
    // cpapi recon tickle get an immediate connection-refused.
    let v: HashMap<String, String> = HashMap::new();
    // The gateway is the DEMO tier's `venue.ibkr.demo.{backend,port,cpapi_url}` rows (decision
    // 0095), which reach the mount through `MountPolicy::venue_settings`, never the credential map.
    let gateway: Vec<vike_secrets::VenueSettingRow> =
        [("BACKEND", "socket"), ("PORT", "65534"), ("CPAPI_URL", "https://127.0.0.1:65533")]
            .into_iter()
            .map(|(field, value)| vike_secrets::VenueSettingRow {
                venue: "ibkr".to_string(),
                tier: Some(AccountTier::Demo),
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
    // not run. ibkr's `resolve` reads `load_ibkr_config_for_account(Demo, …)`, which the
    // vars above satisfy, and the `demo` account row below arms it — so ibkr IS live-intent here, and with no
    // budget the mount refuses without ever reaching the connect failure this test exists
    // to observe. (Measured: on this test's first ever execution, once the `ibkr` CI lane widened
    // to compile it, that is exactly how it failed.)
    //
    // fxcm's row answers `(None, SdkAbsent)` on its FIRST conjunct — `sdk_available()` is false
    // on every CI runner — so its twin is not live-intent and never reaches the budget gate.
    // ibkr has no such conjunct available: its Gateway's liveness is knowable ONLY by attempting a
    // connect, which is precisely the I/O this gate exists to precede.
    //
    // The two caps below are the exact pair `require_live_risk_budget` reads, and nothing else is
    // set — a profile carrying no venue-owned field arms the operator budget and nothing more.
    let budget = vike_model::ProfileRisk {
        max_notional_per_order: Some(100.0),
        max_total_exposure: Some(500.0),
        ..Default::default()
    };
    // ⚠ ibkr's ACCOUNT must be armed (an active `demo` row holding its `ACCOUNT` field), or the
    // bridge is never reached at all and this test passes against the paper early return — every assertion here is also true
    // of a paper-held mount, so without this line it would be vacuous rather than red.
    let policy = MountPolicy {
        venue_settings: std::collections::BTreeMap::from([(
            "ibkr".to_string(),
            vike_secrets::venue_setting::VenueSettings::from_rows("ibkr", &gateway),
        )]),
        ..MountPolicy::default().with_credentials(
            "ibkr",
            &AccountLabel::Default,
            AccountTier::Demo,
            &[("ACCOUNT", "DUTEST000")],
        )
    };
    let mut env = vike_mount::MountEnv::new(REGISTRY, &v, &tx, &mut live);
    // `recon_enabled: true` on purpose — the cpapi handshake must actually be reached and FAIL on
    // a dead port; `false` would stop it at the global gate and pin nothing.
    env.recon_enabled = true;
    env.risk_profile = Some(&budget);
    env.policy = Some(&policy);
    let stopped = match vike_mount::make_engine(&mut env, "ibkr", "AAPL.SMART.USD") {
        Err(vike_mount::MountError::NotRunning(stopped)) => stopped,
        Err(other) => {
            panic!("budget supplied + no reachable Gateway: expected NOT RUNNING: {other}")
        }
        Ok((engine, _)) => panic!(
            "no reachable Gateway built an engine at {} — an account whose connect failed must \
             not run, and no paper engine stands in for it",
            engine.tier
        ),
    };
    assert_eq!(
        (stopped.tier, stopped.block),
        (Some(AccountTier::Demo), ArmingBlock::MountFailed),
        "connect failure: the account does not run, and says its mount failed"
    );
    assert!(live.is_empty(), "connect failure → not marked live");
}

/// MOVED from `vike-mount`'s `tests/account_credential_isolation.rs` (its ibkr case): the 2×2 over
/// the REAL projection — `venue_account_arming`, the one `make_engine_accounts` selects with —
/// plus the two equalities that file held every case to: a second account's field perturbs nothing
/// about the first account's row, and an unconfigured labelled account reads like an unconfigured
/// default one. (`ACCOUNT` is ibkr's only field, so the half-written half of that file's last test
/// never applied to it.)
#[test]
fn a_labelled_ibkr_account_reads_only_its_own_keys() {
    // Two `demo` rows (the DEFAULT account and ALT), each holding `ACCOUNT` or nothing.
    let policy = |default: Option<&str>, labelled: Option<&str>| -> MountPolicy {
        let mut p = MountPolicy::default()
            .with_account("ibkr", &AccountLabel::Default, AccountTier::Demo)
            .with_account("ibkr", &alt(), AccountTier::Demo);
        for (label, value) in [(AccountLabel::Default, default), (alt(), labelled)] {
            if let Some(value) = value {
                p = p.with_credentials("ibkr", &label, AccountTier::Demo, &[("ACCOUNT", value)]);
            }
        }
        p
    };
    let row = |label: &AccountLabel, policy: &MountPolicy| -> VenueArming {
        vike_mount::venue_account_arming(REGISTRY, "ibkr", &HashMap::new(), Some(policy))
            .into_iter()
            .find(|r| r.label == *label)
            .unwrap_or_else(|| panic!("ibkr: no row for {label}"))
    };

    let default_only = policy(Some("DU1234567"), None);
    assert_eq!(row(&AccountLabel::Default, &default_only).effective, Some(AccountTier::Demo));
    let leaked = row(&alt(), &default_only);
    assert_eq!(
        (leaked.effective, leaked.block),
        (None, ArmingBlock::NoCredentials),
        "ALT armed off the DEFAULT account's `ACCOUNT` — a second engine on the first book"
    );

    let alt_only = policy(None, Some("DU1234567"));
    assert_eq!(
        row(&alt(), &alt_only).effective,
        Some(AccountTier::Demo),
        "ALT's own field arms ALT"
    );
    let lost = row(&AccountLabel::Default, &alt_only);
    assert_eq!((lost.effective, lost.block), (None, ArmingBlock::NoCredentials));

    let beside = policy(Some("DU1234567"), Some("DU1234567-second-account"));
    assert_eq!(
        row(&AccountLabel::Default, &default_only),
        row(&AccountLabel::Default, &beside),
        "a second account's field must perturb nothing about the first account's row"
    );

    let empty = policy(None, None);
    assert_eq!(
        row(&alt(), &empty).block,
        row(&AccountLabel::Default, &empty).block,
        "an unconfigured labelled account must read like an unconfigured default one"
    );
}

/// MOVED from `vike-mount`'s `tests/book_identity_table.rs` (`every_named_row_resolves_from_its_own_keys`):
/// ibkr's `Named` row resolves from the account's `ACCOUNT` field. It left that file because
/// `vike-mount`'s own registry now answers ibkr's book with the generic `FeatureAbsent` row. The
/// field carries no tier (decision 0156 item 6): the mount passes the mode, and a different account
/// holding its own `ACCOUNT` has its own book.
#[test]
fn the_ibkr_book_is_named_by_its_own_account_key() {
    for (mode, value, expected) in [
        (AccountTier::Demo, "DUQ186573", "duq186573"),
        (AccountTier::Live, "U13112916", "u13112916"),
    ] {
        let held = fields(&[("ACCOUNT", value)]);
        assert_eq!(
            effective_book(
                REGISTRY,
                "ibkr",
                mode,
                AccountCredentials::new(Some(1), Some(&held), None)
            )
            .as_deref(),
            Some(expected),
            "ibkr: the ACCOUNT field must name the book at {mode:?}"
        );
        // …and an account holding nothing has no book: the no-fallback rule every credential
        // loader in this workspace obeys.
        assert_eq!(
            effective_book(REGISTRY, "ibkr", mode, AccountCredentials::none()),
            None,
            "ibkr: an account holding no ACCOUNT field names no book"
        );
    }
}

/// **IBKR's stay-paper matrix: every account tier x every credential state, pinned cell by cell** —
/// the feature-on twin of `mount_roster/stay_paper_matrix.rs`, which can only see ibkr as a
/// `FeatureAbsent` row. The arm resolves the account's own tier: a `demo` account holding
/// `ACCOUNT` arms the paper-account session, and a `live` account holding it is PAPER with
/// `LiveTierNotWired` (the arm cannot trade live and says so), never the demo session it once
/// fell back to — a live account never trades demo (decision 0119). An account holding nothing is
/// `NoCredentials` at either tier, silently.
/// (`crates/bridges/vike-ibkr/src/mount/tests.rs` holds the arming cases.)
///
/// Offline: the arming projection is the pin. An armed cell would dial a Gateway, so it is not
/// mounted here (`ibkr_present_config_without_gateway_does_not_run` is the mounted twin).
#[test]
fn the_ibkr_stay_paper_matrix() {
    let held = fields(&[("ACCOUNT", "DU1234567")]);
    let nothing = CredentialFields::new();
    type Cell = (Option<AccountTier>, ArmingBlock);
    let none: Cell = (None, ArmingBlock::NoCredentials);
    // A LIVE-tier account the arm will not use: does not run, and says which tier it found.
    let unwired: Cell = (None, ArmingBlock::LiveTierNotWired);
    let cases: [(&CredentialFields, Cell, Cell); 2] =
        [(&nothing, none, none), (&held, (Some(AccountTier::Demo), ArmingBlock::None), unwired)];
    for (fields, at_demo, at_live) in cases {
        for (tier, expected) in [(AccountTier::Demo, at_demo), (AccountTier::Live, at_live)] {
            assert_eq!(
                vike_mount::venue_arming_under(
                    REGISTRY,
                    "ibkr",
                    &HashMap::new(),
                    AccountCredentials::new(Some(1), Some(fields), None),
                    tier
                ),
                expected,
                "ibkr: holding {:?} for a `{tier}` account",
                fields.fields().collect::<Vec<_>>()
            );
        }
    }
    // …and a `paper` tier answers above the bridge whatever the account holds.
    assert_eq!(
        vike_mount::venue_arming_under(
            REGISTRY,
            "ibkr",
            &HashMap::new(),
            AccountCredentials::new(Some(1), Some(&held), None),
            AccountTier::Paper
        ),
        (Some(AccountTier::Paper), ArmingBlock::PaperTier)
    );
}
