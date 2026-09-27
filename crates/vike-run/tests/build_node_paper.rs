//! PR-8 gate: [`vike_run::build_node`] mounts the PAPER fallback for every venue over an EMPTY
//! credentials map. Absent credentials ARE the live gate, so this needs NO network and NO creds — the
//! strongest CI-able proof that the twelve-venue mount assembly moved out of `vike-app/main.rs` still
//! stands up the same live core. It is the whole-assembly twin of vike-mount's
//! `all_roster_venues_absent_creds_stay_paper_and_inert` (which proves each `make_engine` arm in
//! isolation): here the arms run through `build_node` → `spawn_core_multi` + the recon-client list + the
//! live-event forwarder, end to end.
//!
//! The startup-preflight wiring extends the same gate: `build_node` now runs
//! `vike_mount::startup::run_startup_preflight` first, and BOTH properties the mount depends on are
//! asserted here — that an empty-creds preflight is offline and empty-handed
//! (`the_startup_preflight_over_empty_creds_is_offline`), and that even a HARD-FAILING report is
//! advisory, never an abort (`a_failing_preflight_does_not_abort_a_paper_mount`).

use std::collections::HashMap;

use vike_mount::preflight::{
    CHECK_CREDENTIALS, CHECK_NETWORK, CheckReport, CheckStatus, PreflightReport, VenueDisposition,
};
use vike_run::{NodeConfig, build_node, build_node_with_preflight};

/// The `NodeConfig` every test here mounts: empty `.env` ⇒ absent creds for every venue ⇒ paper
/// (no network). The `default()` `CoreConfig` opens no files, mounts no strategy, and arms no
/// timer, so the spawn is self-contained.
fn paper_cfg() -> NodeConfig {
    NodeConfig {
        vars: HashMap::new(),
        properties_rec: None, // recorder construction is the binary's job; None = disabled path
        seed_cash: 10_000.0,
        recon_enabled: false,
        core_config: vike_core::CoreConfig::default(),
        risk_profile: None, // no operator profile configured — byte-identical to pre-wiring
        // No `policy.toml` on this machine. `MountPolicy::default()` IS that answer (every field
        // `None` ⇒ every venue keeps its own compiled-in literal), which is what makes this whole
        // suite the byte-identical gate for the Phase-6c policy wiring too: the assertions below
        // are unchanged because the mount is unchanged.
        policy: vike_run::MountPolicy::default(),
    }
}

/// The paper-mount assertions every test here shares, plus the clean teardown in the caller's
/// load-bearing order: raise the forwarder stop BEFORE the core shutdown+join (see
/// `vike_run::node`'s forwarder teardown-safety note).
fn assert_paper_and_teardown(node: vike_run::Node) {
    assert!(node.recon_clients.is_empty(), "paper mount → no venue has a reconcile handle");
    assert!(node.live_venues.is_empty(), "paper mount → no venue marked live");
    assert!(node.recon_trigger.is_none(), "recon disabled → no reconnect-trigger channel");
    assert!(node.handle.is_alive(), "the single-writer core thread spawned and is running");

    node.forwarder_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    node.handle.shutdown_and_join();
}

/// Empty `.env` ⇒ every venue paper: no live client (so `live_venues` is empty), hence no reconcile
/// handle for any of them (so `recon_clients` is empty); `recon_enabled: false` ⇒ no trigger channel;
/// and the single-writer core thread spawned and is running.
#[test]
fn build_node_over_empty_creds_mounts_all_paper_and_the_core_is_live() {
    let node = build_node(paper_cfg()).expect("paper node builds with no creds and no network");
    assert_paper_and_teardown(node);
}

/// Settings-unification Phase 6c, the whole-assembly half: threading a NON-default machine policy
/// through all twelve arms changes NOTHING about a paper mount, and — the part that matters for a
/// suite that runs in CI with no credentials — costs no network. The policy binds a LIVE
/// hyperliquid exec client's emulated-market band and nothing else; with no key in the map that arm
/// never builds a client at all, so the assembly's shape is identical to the test above.
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
        policy: vike_run::MountPolicy {
            market_slippage: Some(0.002),
            halt_admit: vike_model::HaltAdmit::Verify,
            // ⚠ `..Default::default()` for the ARMING CEILING, deliberately left at its default:
            // that default is `paper` for every venue, which is what a machine with no
            // `policy.toml` gets and what this whole suite is the byte-identical gate for. With no
            // credentials in `vars` the mount is paper either way, so the assertions below are
            // unchanged — but a future editor arming a venue here would be changing the subject.
            ..Default::default()
        },
        ..paper_cfg()
    };
    let node = build_node(cfg).expect("a machine policy must never make a paper mount refuse");
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
    // `None` is the all-`paper` arming ceiling, which is also what `paper_cfg()`'s
    // `MountPolicy::default()` carries — so this is the report `build_node` would produce here.
    let report = vike_mount::startup::run_startup_preflight(&HashMap::new(), &[], None);
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
/// per-venue FAIL that marks binance `Paper`.
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

/// THE degrade-not-abort property: a report that hard-fails BOTH ways must still mount the node.
/// `build_node_with_preflight` is what `build_node` itself calls with the real report, so this pins
/// the half of the decision that did NOT change when the disposition became enforced: a GLOBAL
/// no-go is logged and the assembly proceeds, because a daemon that refuses to start crash-loops
/// under `Restart=on-failure`.
#[test]
fn a_failing_preflight_does_not_abort_a_paper_mount() {
    let report = hard_failing_report();
    assert!(!report.go(), "precondition: the report is a hard NO-GO");
    assert_eq!(report.venue_disposition("binance"), VenueDisposition::Paper, "precondition");

    let node = build_node_with_preflight(paper_cfg(), &report)
        .expect("a failing preflight degrades to a logged report — it never aborts the mount");
    assert_paper_and_teardown(node);
}

/// THE enforcement, at the assembly: a per-venue FAIL must reach the MOUNT, not just the log.
///
/// The mechanism is credential withholding (`vike_mount::startup::withhold_venue_credentials`), so
/// the observable is the credential map `build_node_inner` reads: binance's keys are gone by the
/// time any arm runs, and every other venue's survive. That is what makes `make_engine` land on the
/// paper fallback through the ordinary live gate rather than through a second, parallel switch.
///
/// ⚠ This is asserted on the MAP rather than on `node.live_venues` deliberately: a live binance
/// mount would need real credentials and would dial the venue, which no CI job may do. The map is
/// where the decision is taken; `vike_mount`'s own
/// `a_withheld_venue_would_no_longer_mount_live` closes the other half (withheld keys ⇒
/// `would_mount_live` is false) inside the crate that can see that predicate.
#[test]
fn a_per_venue_preflight_fail_withholds_that_venues_credentials() {
    let mut vars: HashMap<String, String> = [
        ("BINANCE_DEMO_API_KEY", "k"),
        ("BINANCE_DEMO_API_SECRET", "s"),
        ("BINANCE_BROKER_CODE", "x-abc"),
        ("BYBIT_DEMO_API_KEY", "k"),
        ("BYBIT_DEMO_API_SECRET", "s"),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
    .collect();

    let report = hard_failing_report();
    assert_eq!(report.degraded_venues(), vec!["binance".to_string()], "precondition");

    // The same call `build_node_with_preflight` makes, over the same map.
    for venue in report.degraded_venues() {
        vike_mount::startup::withhold_venue_credentials(&mut vars, &venue);
    }

    assert!(
        !vars.keys().any(|k| k.starts_with("BINANCE_")),
        "the degraded venue keeps NO key — absent credentials ARE the live gate: {:?}",
        vars.keys().collect::<Vec<_>>()
    );
    assert!(
        vars.contains_key("BYBIT_DEMO_API_KEY") && vars.contains_key("BYBIT_DEMO_API_SECRET"),
        "a venue the preflight did not fail must be untouched — preflight only ever DEMOTES"
    );
}
