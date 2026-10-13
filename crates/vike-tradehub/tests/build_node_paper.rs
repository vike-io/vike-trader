//! PR-8 gate: [`vike_mount::build_node`] mounts every wired market's `paper` account over an EMPTY
//! credentials map. Absent credentials ARE the live gate, so this needs NO network and NO creds — the
//! strongest CI-able proof that the node mount assembly moved out of `vike-app/main.rs` still
//! stands up the same live core. It is the whole-assembly twin of
//! `crates/vike-tradehub/tests/mount_roster/fee_schedule.rs`'s per-venue paper mounts: here the
//! mounts run through `build_node` → `spawn_core_multi`, the recon-client list and the live-event
//! forwarder, end to end.
//!
//! ⚠ **Every account here is a `paper` ROW** (decision 0156): an account runs at the tier its active
//! row states or not at all, so a box whose table names no account runs NOTHING — and the node,
//! which needs a primary engine, is refused naming every account and its reason
//! (`build_node_over_an_empty_account_table_runs_nothing_and_says_why`). No paper engine stands in
//! for an account that does not run.
//!
//! The startup-preflight wiring extends the same gate: `build_node` now runs
//! `vike_mount::startup::run_startup_preflight` first, and BOTH properties the mount depends on are
//! asserted here — that an empty-creds preflight is offline and empty-handed
//! (`the_startup_preflight_over_empty_creds_is_offline`), and that even a HARD-FAILING report is
//! advisory, never an abort, for the accounts it does not stop
//! (`a_failing_preflight_does_not_abort_a_paper_mount`).
//!
//! Moved from `crates/vike-run/tests/` when the venue mount contract finished
//! (docs/decisions/0096): `vike-run` held no registry, so it ran through the transitional one until
//! then. Default build only: `vike-mount`'s transitional registry carried ibkr, fxcm and polymarket
//! `FeatureAbsent` in every build and these assertions were written against that; each venue's
//! feature-on half is its own `crates/vike-tradehub/tests/ibkr_mount.rs`,
//! `crates/vike-tradehub/tests/fxcm_mount.rs` or `crates/vike-tradehub/tests/polymarket_mount.rs`.
//! That crate-level `#![cfg]` also makes this its own test binary rather than a `daemon` member.
//! ⚠ It narrows where this runs: in `crates/vike-run/tests/` it also ran in the fxcm, ibkr and
//! polymarket-stack lanes, with vike-run's marker feature on. It runs in the default lane only
//! now; `crates/vike-tradehub/tests/daemon/live_gate_paper.rs` still drives `build_node` in those
//! lanes.
#![cfg(not(any(feature = "ibkr", feature = "polymarket", feature = "fxcm")))]

use std::collections::HashMap;

use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_mount::preflight::{
    CHECK_CREDENTIALS, CHECK_NETWORK, CheckReport, CheckStatus, PreflightReport, VenueDisposition,
};
use vike_mount::{NodeConfig, NodeError, build_node, build_node_with_preflight};

/// The account table every test here mounts under: ONE active `paper` row for the DEFAULT account
/// of every wired market's venue — the table a paper box has.
fn paper_policy() -> vike_mount::MountPolicy {
    vike_tradehub::wired_markets::WIRED_MARKETS.iter().fold(
        vike_mount::MountPolicy::default(),
        |policy, market| {
            policy.with_account(market.venue, &AccountLabel::Default, AccountTier::Paper)
        },
    )
}

/// The `NodeConfig` every test here mounts: empty `.env` ⇒ absent creds for every venue, and every
/// account a `paper` row ⇒ the simulator (no network). The `default()` `CoreConfig` opens no
/// files, mounts no strategy, and arms no timer, so the spawn is self-contained.
fn paper_cfg() -> NodeConfig {
    NodeConfig {
        registry: vike_tradehub::registry::REGISTRY,
        markets: vike_tradehub::wired_markets::WIRED_MARKETS,
        vars: HashMap::new(),
        properties_rec: None, // recorder construction is the binary's job; None = disabled path
        seed_cash: 10_000.0,
        recon_enabled: false,
        core_config: vike_core::CoreConfig::default(),
        risk_profile: None, // no operator profile configured — byte-identical to pre-wiring
        // No `policy` setting rows on this machine, only the `paper` account rows: every other
        // field `None` ⇒ every venue keeps its own compiled-in literal, which is what makes this
        // whole suite the byte-identical gate for the Phase-6c policy wiring too.
        policy: paper_policy(),
    }
}

/// The paper-mount assertions every test here shares, plus the clean teardown in the caller's
/// load-bearing order: raise the forwarder stop BEFORE the core shutdown+join (see
/// `crates/vike-mount/src/node.rs`'s forwarder teardown-safety note).
fn assert_paper_and_teardown(node: vike_mount::Node) {
    assert!(node.recon_clients.is_empty(), "paper mount → no venue has a reconcile handle");
    assert!(node.live_venues.is_empty(), "paper mount → no venue marked live");
    assert!(node.recon_trigger.is_none(), "recon disabled → no reconnect-trigger channel");
    assert!(node.not_running.is_empty(), "every account runs: {:?}", node.not_running);
    assert!(node.handle.is_alive(), "the single-writer core thread spawned and is running");

    node.forwarder_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    node.handle.shutdown_and_join();
}

/// Empty `.env` and a `paper` row per venue ⇒ every venue paper: no live client (so `live_venues` is
/// empty), hence no reconcile handle for any of them (so `recon_clients` is empty);
/// `recon_enabled: false` ⇒ no trigger channel; and the single-writer core thread spawned and is
/// running.
#[test]
fn build_node_over_empty_creds_mounts_all_paper_and_the_core_is_live() {
    let node = build_node(paper_cfg())
        .unwrap_or_else(|e| panic!("paper node builds with no creds and no network: {e}"));
    assert_paper_and_teardown(node);
}

/// **Decision 0156 at the assembly: an account table that names no account runs NOTHING.** The
/// default account of every wired market does not run (`NoAccountRow`), no paper engine stands in
/// for any of them, and the node — which needs a primary engine — is refused, naming every account
/// and why. Before the decision this box mounted a paper engine per venue under the venue's name.
#[test]
fn build_node_over_an_empty_account_table_runs_nothing_and_says_why() {
    let cfg = NodeConfig { policy: vike_mount::MountPolicy::default(), ..paper_cfg() };
    let not_running = match build_node(cfg) {
        Err(NodeError::NoAccountRuns { not_running }) => not_running,
        Err(other) => panic!("expected the no-account refusal, got: {other}"),
        Ok(node) => {
            node.forwarder_stop.store(true, std::sync::atomic::Ordering::Relaxed);
            node.handle.shutdown_and_join();
            panic!("a box whose table names no account built a node")
        }
    };
    let mut venues: Vec<&str> = not_running.iter().map(|nr| nr.venue.as_str()).collect();
    venues.sort_unstable();
    venues.dedup();
    let mut wired: Vec<&str> =
        vike_tradehub::wired_markets::WIRED_MARKETS.iter().map(|m| m.venue).collect();
    wired.sort_unstable();
    wired.dedup();
    assert_eq!(venues, wired, "every wired venue's account is named, and none runs");
    for nr in &not_running {
        assert_eq!(
            (nr.tier, nr.block),
            (None, vike_config::ArmingBlock::NoAccountRow),
            "{}: no row, so no tier and no engine",
            nr.venue
        );
    }
}

/// Settings-unification Phase 6c, the whole-assembly half: threading a NON-default machine policy
/// through every wired market changes NOTHING about a paper mount, and — the part that matters for
/// a suite that runs in CI with no credentials — costs no network. The policy binds a LIVE
/// hyperliquid exec client's emulated-market band and nothing else; every account is a `paper` row,
/// so that arm never builds a client at all, and the assembly's shape is identical to the test
/// above.
///
/// This is the guard against the plausible wrong wiring — a policy value leaking into the
/// `RiskLimits` / venue-grid path, where it would have silently changed every venue including the
/// paper ones. `assert_paper_and_teardown` failing here would be exactly that bug.
#[test]
fn a_machine_policy_leaves_the_paper_mount_untouched() {
    let cfg = NodeConfig {
        // A real, in-range operator band (0.2 %) AND the non-default halt-admit mode — neither is a
        // default, so a leak into a paper mount would show. `verify` is a good stress here: it is
        // real on exactly one venue and must degrade quietly (with one log line) on every paper one.
        policy: vike_mount::MountPolicy {
            market_slippage: Some(0.002),
            halt_admit: vike_model::HaltAdmit::Verify,
            // ⚠ The ACCOUNT TABLE is the paper one: every account a `paper` row. A future editor
            // arming a venue here would be changing the subject.
            ..paper_policy()
        },
        ..paper_cfg()
    };
    let node = build_node(cfg)
        .unwrap_or_else(|e| panic!("a machine policy must never make a paper mount refuse: {e}"));
    assert_paper_and_teardown(node);
}

/// The exact preflight `build_node` runs, over the exact vars a paper mount passes it: it produces
/// a report (the seam is genuinely mounted, not merely present), and that report is OFFLINE and
/// empty-handed — only the global network row, and NO per-venue leg at all because no credential
/// resolves. This is what keeps the test above network-free. The network row is NOT APPLICABLE
/// rather than a WARN: no `NetProbe` is spawned when no venue would mount live, and with nothing
/// live there is no order path whose connectivity could be at stake. It must still never read as a
/// measurement that was taken.
#[test]
fn the_startup_preflight_over_empty_creds_is_offline() {
    // `None` arms nothing (no account runs live), and `paper_cfg()`'s table arms nothing live
    // either — so this is the report `build_node` would produce here.
    let report = vike_mount::startup::run_startup_preflight(
        vike_tradehub::registry::REGISTRY,
        &HashMap::new(),
        &[],
        None,
    );
    // A dev box may export the skip flag; then the empty report IS the whole contract.
    if report.skipped {
        assert!(report.checks.is_empty(), "a skipped preflight runs no check at all");
        return;
    }
    assert_eq!(report.checks.len(), 1, "network only — no venue is checked: {:?}", report.lines());
    assert_eq!(report.checks[0].name, CHECK_NETWORK);
    assert_eq!(report.checks[0].status, CheckStatus::NotApplicable);
    assert_ne!(report.checks[0].status, CheckStatus::Pass, "never a fake PASS");
    assert!(report.go());
    assert!(report.degraded_venues().is_empty());
}

/// The hard-failing report both tests below drive: a GLOBAL no-go (`go() == false`) AND a
/// per-venue FAIL that STOPS binance's `demo` and `live` accounts (`VenueDisposition::Stopped`).
fn hard_failing_report() -> PreflightReport {
    PreflightReport {
        checks: vec![
            CheckReport {
                name: CHECK_NETWORK.to_string(),
                venue: None, // venue-less ⇒ a GLOBAL failure ⇒ go() == false
                status: CheckStatus::Fail,
                message: "internet DOWN since epoch ms 1700000000000".to_string(),
                remediation: "check the local resolver".to_string(),
            },
            CheckReport {
                name: CHECK_CREDENTIALS.to_string(),
                venue: Some("binance".to_string()), // venue-scoped ⇒ degrades that venue
                status: CheckStatus::Fail,
                message: "authenticated read rejected: 401".to_string(),
                remediation: "check this venue's keys".to_string(),
            },
        ],
        skipped: false,
    }
}

/// THE degrade-not-abort property: a report that hard-fails BOTH ways must still mount the node —
/// its `paper` accounts run whatever the report says (the stop is for `demo` and `live` ones).
/// `build_node_with_preflight` is what `build_node` itself calls with the real report, so this pins
/// the half of the decision that did NOT change when the disposition became enforced: a GLOBAL
/// no-go is logged and the assembly proceeds, because a daemon that refuses to start crash-loops
/// under `Restart=on-failure`.
#[test]
fn a_failing_preflight_does_not_abort_a_paper_mount() {
    let report = hard_failing_report();
    assert!(!report.go(), "precondition: the report is a hard NO-GO");
    assert_eq!(report.venue_disposition("binance"), VenueDisposition::Stopped, "precondition");

    let node = build_node_with_preflight(paper_cfg(), &report)
        .expect("a failing preflight degrades to a logged report — it never aborts the mount");
    assert_paper_and_teardown(node);
}

/// THE enforcement, at the assembly: a per-venue FAIL must reach the MOUNT, not just the log.
///
/// The mechanism is credential withholding (`vike_mount::startup::withhold_venue_credentials`), so
/// the observable is the account directory `build_node_inner` reads: binance's credentials are gone by
/// the time any arm runs, and every other venue's survive. That is what makes `make_engine` land on the
/// paper fallback through the ordinary live gate rather than through a second, parallel switch.
///
/// ⚠ This is asserted on the MAP rather than on `node.live_venues` deliberately: a live binance
/// mount would need real credentials and would dial the venue, which no CI job may do. The map is
/// where the decision is taken; `vike_mount`'s own
/// `a_withheld_venue_would_no_longer_mount_live` closes the other half (withheld keys ⇒
/// `would_mount_live` is false) inside the crate that can see that predicate.
#[test]
fn a_per_venue_preflight_fail_withholds_that_venues_credentials() {
    use vike_model::AccountTier;
    use vike_model::accounts::account_keys::AccountLabel;
    let policy = vike_mount::MountPolicy::default()
        .with_credentials(
            "binance",
            &AccountLabel::Default,
            AccountTier::Demo,
            &[("API_KEY", "k"), ("API_SECRET", "s")],
        )
        .with_credentials(
            "bybit",
            &AccountLabel::Default,
            AccountTier::Demo,
            &[("API_KEY", "k"), ("API_SECRET", "s")],
        );
    let mut accounts = policy.accounts;

    let report = hard_failing_report();
    assert_eq!(report.degraded_venues(), vec!["binance".to_string()], "precondition");

    // The same call `build_node_with_preflight` makes, over the same directory.
    for venue in report.degraded_venues() {
        let (kept, withheld) = vike_mount::startup::withhold_venue_credentials(&accounts, &venue);
        assert_eq!(withheld, 2, "binance held two fields");
        accounts = kept;
    }

    let held =
        |venue: &str| accounts.credentials_for(venue, &AccountLabel::Default, AccountTier::Demo);
    assert!(
        !held("binance").holds_any(),
        "the degraded venue keeps NO credential — absent credentials ARE the live gate"
    );
    assert!(
        held("bybit").get("API_KEY").is_some() && held("bybit").get("API_SECRET").is_some(),
        "a venue the preflight did not fail must be untouched — preflight only ever DEMOTES"
    );
}
