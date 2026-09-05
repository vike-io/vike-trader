//! RunProfile operator-risk-budget wiring over the PAPER mount (Settings STEP 2 PR 1, Task 2).
//!
//! Proves the two properties the plan's merge-safety rule demands, over the REAL mounted core
//! `main.rs` builds (not just the pure resolver — see `config.rs`'s `resolve_paper_risk_limits`
//! unit tests for that half):
//!
//! 1. **No profile ⇒ byte-identical.** `resolve_paper_risk_limits(None)` is `RiskLimits::new()`
//!    verbatim, and a mount built from it admits an order that would be denied under ANY notional
//!    cap — proving nothing is silently enforced today.
//! 2. **A profile's `[risk]` budget ARMS the gate.** An order that violates
//!    `max_notional_per_order` is DENIED (`Event::OrderDenied`, surfaced in `recent_events`) and
//!    never enters the order registry — over `vike_run::build_paper_maker_core_with` with
//!    `PaperMountOpts::risk_limits` (audit F12 collapsed the former `_with_risk_limits` twin into
//!    the one options variant), the exact entry point `main.rs` now calls.
//!
//! No network, no creds — same shape as `tests/headless_lifecycle.rs`.

use std::time::{Duration, Instant};

use vike_core::RunProfile;
use vike_exec::{Command, OrderIntent};
use vike_model::OrderRequest;
use vike_run::{
    MakerMountConfig, PaperMountOpts, build_paper_maker_core, build_paper_maker_core_with,
};
use vike_tradehub::config::resolve_paper_risk_limits;

/// The exact shape `main.rs` mounts with: only the operator risk budget armed, every other opt-in
/// knob at its byte-identical default.
fn mount_with_risk(cfg: &MakerMountConfig, limits: vike_exec::RiskLimits) -> vike_run::MakerMount {
    build_paper_maker_core_with(cfg, PaperMountOpts { risk_limits: limits, ..Default::default() })
}

const TOKEN: &str = "RUN_PROFILE_RISK_WIRING_TOKEN";
/// Far-future resolution so the A-S horizon is positive (mirrors the other mount tests).
const RESOLUTION_TS: i64 = 3_000_000_000;

/// Poll `cond` up to `secs` — the core folds on its own thread and publishes coalesced snapshots.
fn wait_until(secs: u64, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn submit(
    handle: &vike_core::CoreHandle,
    cfg: &MakerMountConfig,
    coid: &str,
    qty: f64,
    price: f64,
) {
    let req = OrderRequest {
        client_order_id: coid.to_string(),
        venue: cfg.venue.clone(),
        symbol: cfg.token_id.clone(),
        side: 1,
        qty,
        order_type: "limit".to_string(),
        price: Some(price),
        ..Default::default()
    };
    handle.send_command(Command::Order(OrderIntent::Submit(Box::new(req))));
}

#[test]
fn no_profile_admits_a_large_order_exactly_like_the_plain_paper_builder() {
    vike_log::test_init();
    let cfg = MakerMountConfig::polymarket(TOKEN, Some(RESOLUTION_TS));

    // The exact value `main.rs` computes absent --profile/VIKE_RUN_PROFILE.
    let limits = resolve_paper_risk_limits(None).expect("no profile is never an error");
    assert_eq!(limits, vike_exec::RiskLimits::new(), "no profile must be byte-identical to today");

    let mount = mount_with_risk(&cfg, limits);
    let coid = "big-order";
    // qty(10_000) * price(0.99) is a huge notional that WOULD be denied under any sane cap — proves
    // the absent-profile path truly enforces nothing beyond the venue lot grid.
    submit(&mount.handle, &cfg, coid, 10_000.0, 0.99);

    assert!(
        wait_until(5, || mount.handle.snapshot().order(coid).is_some()),
        "an order must be ADMITTED with no profile configured (no notional cap armed)"
    );
    let snap = mount.handle.snapshot();
    assert!(
        !snap.recent_events.iter().any(|e| e.contains("OrderDenied")),
        "no profile ⇒ no denial: {:?}",
        snap.recent_events
    );
    assert!(snap.fault.is_none());

    mount.handle.shutdown_and_join();
}

#[test]
fn risk_limits_new_via_opts_equals_the_plain_paper_builder() {
    // Extra belt-and-suspenders on the merge-safety property at the vike-run seam itself: feeding
    // `build_paper_maker_core_with` the SAME `RiskLimits::new()` that `PaperMountOpts::default()`
    // (hence the plain `build_paper_maker_core`) carries must behave identically — both must build
    // and tear down cleanly with no fault.
    vike_log::test_init();
    let cfg = MakerMountConfig::polymarket(TOKEN, Some(RESOLUTION_TS));

    let plain = build_paper_maker_core(&cfg);
    assert!(plain.handle.snapshot().fault.is_none());
    plain.handle.shutdown_and_join();

    let via_risk_seam = mount_with_risk(&cfg, vike_exec::RiskLimits::new());
    assert!(via_risk_seam.handle.snapshot().fault.is_none());
    via_risk_seam.handle.shutdown_and_join();
}

#[test]
fn profile_risk_budget_denies_an_order_that_violates_max_notional_per_order() {
    vike_log::test_init();
    let toml = r#"
mode = "paper"
[event_source]
kind = "live_venue"
venue = "polymarket"
symbol = "TOK"
[broker]
kind = "paper"
seed_cash = 1000.0
[risk]
max_notional_per_order = 5.0
"#;
    let profile = RunProfile::from_toml_str(toml).expect("profile parses and validates");
    let limits =
        resolve_paper_risk_limits(Some(&profile)).expect("NoGridFetched apply_to never errors");
    assert_eq!(limits.max_notional_per_order, Some(5.0), "the operator budget field is armed");

    let cfg = MakerMountConfig::polymarket(TOKEN, Some(RESOLUTION_TS));
    let mount = mount_with_risk(&cfg, limits);

    let coid = "over-cap";
    // qty(20) * price(0.99) = 19.8 notional, far over the 5.0 cap.
    submit(&mount.handle, &cfg, coid, 20.0, 0.99);

    assert!(
        wait_until(5, || {
            let snap = mount.handle.snapshot();
            snap.recent_events.iter().any(|e| e.contains("OrderDenied") && e.contains(coid))
        }),
        "the order must be DENIED by the armed max_notional_per_order budget"
    );
    let snap = mount.handle.snapshot();
    assert!(snap.order(coid).is_none(), "a denied order must never enter the registry");
    assert!(snap.fault.is_none(), "a RiskGate veto must not fault the core: {:?}", snap.fault);

    mount.handle.shutdown_and_join();
}

#[test]
fn profile_risk_budget_still_admits_an_order_that_respects_the_cap() {
    // The mirror of the denial test: the SAME budget does not block a compliant order — proves the
    // gate is discriminating, not just universally rejecting once a profile is present.
    vike_log::test_init();
    let toml = r#"
mode = "paper"
[event_source]
kind = "live_venue"
venue = "polymarket"
symbol = "TOK"
[broker]
kind = "paper"
seed_cash = 1000.0
[risk]
max_notional_per_order = 5.0
"#;
    let profile = RunProfile::from_toml_str(toml).unwrap();
    let limits = resolve_paper_risk_limits(Some(&profile)).unwrap();

    let cfg = MakerMountConfig::polymarket(TOKEN, Some(RESOLUTION_TS));
    let mount = mount_with_risk(&cfg, limits);

    let coid = "under-cap";
    // qty(2) * price(0.40) = 0.80 notional, comfortably under the 5.0 cap.
    submit(&mount.handle, &cfg, coid, 2.0, 0.40);

    assert!(
        wait_until(5, || mount.handle.snapshot().order(coid).is_some()),
        "a compliant order must still be admitted under an armed (but unviolated) cap"
    );

    mount.handle.shutdown_and_join();
}
