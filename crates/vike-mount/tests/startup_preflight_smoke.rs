//! LIVE smoke for the WHOLE startup preflight — both legs, on a box whose credential store is
//! populated — asserting the one property the unit tests structurally cannot see: that the CLOCK
//! leg's budget is not consumed by the CREDENTIAL leg beside it.
//!
//! `#[ignore]`d (it makes network calls) and READ-ONLY: the clock leg is public GETs and the
//! credential leg is each venue's cheapest AUTHED READ (a balance/account fetch). No order is
//! placed, nothing is signed that changes state, and this is the same pass every `vike-tradehub`
//! and `vike-app` start already runs.
//!
//! ```sh
//! # on a box with a populated store — VIKE_SETTINGS_DIR names it when the CWD is not the project
//! cargo test -p vike-mount --test startup_preflight_smoke -- --ignored --nocapture
//! ```
//!
//! # Why this cannot be a unit test
//!
//! `run_preflight` interleaves the two legs per venue, and a credential probe is a blocking authed
//! read on that venue's own 30 s agent. Every budget unit test configures `clock_venues` ALONE and
//! leaves `credential_venues` empty, and their injected clock only advances inside
//! `check_clock_skew` — so the interleaving that caused the defect never happens under them. The
//! defect: on the Windows dev box 2026-08-22 a geo-blocked alpaca probe sat ~20 s on a TCP connect
//! and spent the whole 5 s clock budget, and alpaca/aster/hyperliquid each reported their clock
//! "not read" while the clock leg itself had spent 3.2 s. aster is the one that mattered — it runs
//! against mainnet in practice and its auth binds the clock into the order path.
//!
//! A unit test now pins the ACCOUNTING against a scripted slow probe
//! (`a_slow_credential_probe_does_not_spend_the_clock_budget`). What it cannot pin is that the real
//! venues, at their real latencies, from a box with real credentials, leave every wired clock row
//! actually read — which is what this smoke checks.
use std::collections::HashMap;
use std::time::Instant;

use vike_mount::preflight::{CheckStatus, CHECK_CLOCK_SKEW, CHECK_CREDENTIALS};

/// The marker `check_clock_skew` emits for a venue the budget never reached. Matching on the text
/// is deliberate: this smoke asserts the ABSENCE of an operator-visible line, so it matches what an
/// operator would actually read.
const NOT_READ: &str = "not read";

/// The arming ceiling this smoke runs under: **every roster venue at `live`**, declared explicitly.
///
/// ⚠ Load-bearing, and it is a trap this file has to step around rather than inherit. Since the
/// ceiling reached the preflight, the venue sets are gated on it and `None` means all-`paper` — so
/// a smoke that passed no policy would contact nothing, produce zero credential rows, and exit
/// through the `creds == 0` SKIP below reporting success. The guard is keyed on the very thing the
/// ceiling suppresses, so the vacuous run would look identical to a box with no keys.
///
/// It does NOT read this box's real `policy.toml`: the subject here is the two legs' BUDGET
/// interleaving, which needs both legs to actually run, and a deployment policy that happened to
/// cap a venue would silently shrink the set under test. Widest ceiling, stated once, here.
fn all_live() -> vike_mount::MountPolicy {
    let mut venues = vike_config::VenuePolicy::default();
    for venue in vike_model::VENUES {
        venues = venues.declare(venue, vike_config::VenueMode::Live);
    }
    vike_mount::MountPolicy { venues, ..vike_mount::MountPolicy::default() }
}

/// Venues whose clock row says the budget never reached them.
fn starved(report: &vike_mount::preflight::PreflightReport) -> Vec<String> {
    report
        .checks
        .iter()
        .filter(|c| c.name == CHECK_CLOCK_SKEW && c.message.contains(NOT_READ))
        .map(|c| c.venue.clone().unwrap_or_default())
        .collect()
}

/// THE CONTROL: run the real preflight TWICE — once with the real credential store, once with an
/// EMPTY one — and compare which venues the clock leg reached.
///
/// The empty run has NO credential leg at all, so its starvation is caused by clock reads alone and
/// is the honest baseline. The credentialed run adds an authed read between every pair of clock
/// reads. If those probes are charged to the clock budget (the defect), the credentialed run
/// starves STRICTLY MORE venues than the baseline — which is exactly what the dev box showed: three
/// venues lost their clock check to a geo-blocked alpaca probe that sat ~20 s on a TCP connect.
///
/// Comparing the two runs, rather than asserting "nothing is ever starved", is what makes this
/// honest: the budget legitimately starves venues behind a SLOW CLOCK READ, and on a box where a
/// venue's clock endpoint times out (3 s of a 5 s budget) many venues go unread for reasons that
/// have nothing to do with this fix. That is a real property of the budget's size against the
/// current roster — worth knowing, but not this test's subject.
#[test]
#[ignore = "live: reads every configured venue's clock twice and performs one authed read per credentialed venue"]
fn the_credential_leg_does_not_starve_the_clock_leg() {
    let vars: HashMap<String, String> =
        vike_bridge_core::credentials::load_workspace_secrets_from_env(
            &std::env::vars().collect::<HashMap<String, String>>(),
        );
    println!("credential store: {} keys", vars.len());

    // BASELINE FIRST. ⚠ An empty map yields an EMPTY clock leg, not a credential-free one:
    // `clock_venues` is every venue that would MOUNT LIVE, which needs credentials — so this run
    // reads no clock at all and its starved set is empty by construction. That still makes it a
    // valid floor for the comparison below (nothing starved ⇒ anything starved in the full run is
    // attributable), but it is NOT the "clock reads only" control it looks like, and it is why the
    // full run's starved set is compared against a real budget rather than against this one alone.
    let policy = all_live();
    let t0 = Instant::now();
    let base = vike_mount::startup::run_startup_preflight(&HashMap::new(), &[], Some(&policy));
    let base_ms = t0.elapsed().as_millis();
    if base.skipped {
        println!("SKIP: the preflight skip flag is exported on this box");
        return;
    }

    let t1 = Instant::now();
    let full = vike_mount::startup::run_startup_preflight(&vars, &[], Some(&policy));
    let full_ms = t1.elapsed().as_millis();

    println!("--- empty-store baseline ({base_ms} ms; an empty store reads NO clock) ---");
    for line in base.lines() {
        println!("  {line}");
    }
    println!("--- with the real store ({full_ms} ms) ---");
    for line in full.lines() {
        println!("  {line}");
    }

    let creds = full.checks.iter().filter(|c| c.name == CHECK_CREDENTIALS).count();
    if creds == 0 {
        println!(
            "SKIP: no venue has credentials on this box — the interleaving this smoke exists to \
             check never happens. Point VIKE_SETTINGS_DIR at a populated store."
        );
        return;
    }

    let (base_starved, full_starved) = (starved(&base), starved(&full));
    println!(
        "clock leg unread: {} baseline {:?} vs {} credentialed {:?} ({creds} credential probes, \
         costing {} ms)",
        base_starved.len(),
        base_starved,
        full_starved.len(),
        full_starved,
        full_ms.saturating_sub(base_ms)
    );

    let extra: Vec<&String> = full_starved.iter().filter(|v| !base_starved.contains(v)).collect();
    assert!(
        extra.is_empty(),
        "{} venue(s) {:?} lost their clock check ONLY when the credential leg ran beside it — its \
         probes are being charged to the clock budget. The baseline reached them in {base_ms} ms; \
         the credentialed pass took {full_ms} ms with {creds} probes.",
        extra.len(),
        extra
    );

    // …and the run did real work, so the comparison is not vacuous.
    let measured =
        full.checks.iter().filter(|c| c.name == CHECK_CLOCK_SKEW && c.status == CheckStatus::Pass);
    assert!(measured.count() > 0, "no venue's clock was measured at all — nothing was proven here");
    println!("OK: the credential leg starved no venue the clock leg would otherwise have reached");
}
