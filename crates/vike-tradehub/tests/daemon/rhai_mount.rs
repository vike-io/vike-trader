//! The Rhai twin of `any_strategy_mount.rs`: a profile that names a `.rhai` strategy FILE
//! (`[strategy] file = "<path under strategies/>"` — `docs/decisions/0024-rhai-strategies-live.md`'s
//! rails) mounts on the SAME production core every strategy mounts on, and actually trades on the
//! paper exchange.
//!
//! Same shape as the sibling: `main.rs` is a bin, so this exercises the exact library calls `main`
//! composes — `ProfileDaemon::from_toml_str` → `resolve_strategy` (which for a `.rhai` file reads it,
//! hashes it, judges its header and compiles `vike_rhai::RhaiStrategy<LiveBroker>`) →
//! `vike_tradehub_core::build_paper_strategy_core_with` — then drives CLOSED BARS and asserts a REAL paper
//! fill. The fill's qty is the OVERRIDE the profile's `[strategy.params]` set, which is the
//! end-to-end proof that params flow through `compile_with_params` into the script's own
//! `param("size", …)` — no amount of "it resolved" would show that.
//!
//! The live half of the 0024 rails is here too, network-free on both sides: the pre-connect
//! missing-risk-budget refusal fires for a script mount exactly as for any other (the gate is
//! `vike_tradehub_core`'s, strategy-agnostic, and fires on live INTENT before any venue session exists),
//! and the same script over the same `RuntimeConfig` minus the live intent mounts all-paper — so the
//! refusal is the budget's, not the script's.
//!
//! No network, no creds, no feature flags — default CI lane, like the sibling.

use std::collections::HashMap;
use std::time::Duration;

use vike_execution::BarUpdate;
use vike_marketdata::test_support::flat_bar_zero_volume;
use vike_tradehub::config::ProfileDaemon;
use vike_tradehub_core::{
    RuntimeConfig, build_live_strategy_core_with_preflight, build_paper_strategy_core_with,
};

use crate::strategy_files;
use crate::support::{drive_two_bars, opts_pinned_to, own_sentinel};
use vike_log::poll::wait_until;

const SYMBOL: &str = "RHAI_MOUNT_SYMBOL";
const INTERVAL: &str = "1m";

/// THE headline + the params-flow proof in one: the profile names `buy_hold.rhai`
/// (`crate::strategy_files::BUY_HOLD`: buy `size` once from flat, then hold), its
/// `param("size", 1.0)` is overridden to 3.0 by `[strategy.params]`, and the paper fill is for
/// exactly 3.0 — so the override was baked into the compiled scope, not merely accepted.
#[test]
fn a_rhai_profile_mounts_and_trades_with_the_overridden_size() {
    vike_log::test_init();

    let strategies = strategy_files::standard();
    let profile = ProfileDaemon::from_toml_str(&format!(
        r#"
venue = "polymarket"
symbol = "{SYMBOL}"
interval = "{INTERVAL}"
interval_ms = 60000

[strategy]
file = "buy_hold.rhai"

[strategy.params]
size = 3.0
"#
    ))
    .expect("a rhai profile parses and validates");

    let strategy = profile
        .resolve_strategy(strategies.path(), false)
        .expect("the script compiles and resolves");

    let (_halt_dir, sentinel) = own_sentinel("rhai-mount", "trades");
    let mount = build_paper_strategy_core_with(
        strategy,
        &profile.to_mount_spec(),
        opts_pinned_to(&sentinel),
    );

    // Two CLOSED bars: the first reaches `on_bar` (the script buys SIZE from flat), the paper book
    // fills the market order at the SECOND bar's open — the sibling test's discipline.
    drive_two_bars(&mount.handle, "polymarket", SYMBOL);

    let filled =
        wait_until(Duration::from_secs(10), || !mount.fills.lock().expect("fills").is_empty());
    let fills = mount.fills.lock().expect("fills");
    assert!(filled, "the script never traded: no paper fill after two closed bars");
    let f = &fills[0];
    assert_eq!(
        f.size.to_bits(),
        3.0_f64.to_bits(),
        "the fill must be for the PROFILE's override (3.0), not the script's default (1.0) — \
         params did not flow into the compiled scope"
    );
    assert_eq!(f.side, 1, "the script buys");
    assert_eq!(fills.len(), 1, "the script buys once from flat, then holds");
    drop(fills);

    mount.handle.shutdown_and_join();
}

/// The minimal mountable script: a do-nothing `on_bar` reaches a READY core (alive, folding bars,
/// no fills), and tears down cleanly — the trivial-fixture floor under the trading test above.
#[test]
fn a_do_nothing_rhai_script_reaches_ready() {
    vike_log::test_init();

    let strategies = strategy_files::standard();
    let profile = ProfileDaemon::from_toml_str(&format!(
        "venue = \"polymarket\"\nsymbol = \"{SYMBOL}\"\ninterval = \"{INTERVAL}\"\n\
         interval_ms = 60000\n[strategy]\nfile = \"nothing.rhai\"\n"
    ))
    .expect("a params-free rhai profile parses and validates");

    let strategy =
        profile.resolve_strategy(strategies.path(), false).expect("the do-nothing script compiles");
    let (_halt_dir, sentinel) = own_sentinel("rhai-mount", "does-nothing");
    let mount = build_paper_strategy_core_with(
        strategy,
        &profile.to_mount_spec(),
        opts_pinned_to(&sentinel),
    );
    assert!(mount.handle.is_alive(), "the core thread spawned and is running");

    // Feed it a closed bar so the hook actually runs, then confirm the core is still up and the
    // script traded nothing — a do-nothing script must be a no-op, not a wedge.
    mount
        .handle
        .bar_sender()
        .close(BarUpdate {
            venue: "polymarket".to_string(),
            symbol: SYMBOL.to_string(),
            interval: INTERVAL.to_string(),
            bar: flat_bar_zero_volume(60_000, 0.40),
            sides: None,
        })
        .expect("the core is alive");
    std::thread::sleep(Duration::from_millis(200));
    assert!(mount.handle.is_alive(), "still alive after driving the script's on_bar");
    assert!(mount.fills.lock().expect("fills").is_empty(), "a do-nothing script trades nothing");

    mount.handle.shutdown_and_join();
}

/// The 0024 risk-budget rail, proven at the script mount and network-free on both sides:
///
/// * WITH live intent (a bybit demo account holding its key pair — `would_go_live`'s own gate) and
///   NO operator risk budget, the live build REFUSES pre-connect, naming the missing caps. The
///   gate is `vike_tradehub_core`'s `require_live_risk_budget` — strategy-agnostic, which is exactly the
///   claim: a script cannot slip past it, because the gate never sees what the strategy is.
/// * WITHOUT the live intent (empty vars), the SAME script over the SAME `RuntimeConfig` mounts
///   all-paper — so the refusal above is the budget's, not the script's.
#[test]
fn a_live_rhai_mount_without_a_risk_budget_is_refused_pre_connect() {
    vike_log::test_init();

    // `nothing.rhai`'s header says `runs_on = "live"`, so the live gate's resolve admits it.
    let strategies = strategy_files::standard();
    // The symbol bybit's default account states (`crate::support::live_box`), so the refusal under test is the
    // budget's and not a routing one.
    let profile = ProfileDaemon::from_toml_str(&format!(
        "venue = \"bybit\"\nsymbol = \"BTCUSDT.P\"\ninterval = \"{INTERVAL}\"\n\
         interval_ms = 60000\n[strategy]\nfile = \"nothing.rhai\"\n"
    ))
    .expect("a bybit rhai profile parses and validates");
    assert!(
        profile.validate_for_live(&crate::support::live_box()).is_ok(),
        "bybit/BTCUSDT.P is a live-wired pair — a rhai profile passes the same gate any file does"
    );

    // ⚠ bybit's ACCOUNT must be armed, or there is no live intent for the budget rail to refuse (see
    // the comment on the policy below); the backend is built FROM that same policy.
    let policy_of = |live_intent: bool| {
        let label = vike_model::accounts::account_keys::AccountLabel::Default;
        // The control row is `paper`: an account with no key set at a `demo` tier would not run
        // (decision 0156).
        let policy = vike_tradehub_core::AdapterPolicy::default().with_account(
            "bybit",
            &label,
            if live_intent {
                vike_model::AccountTier::Demo
            } else {
                vike_model::AccountTier::Paper
            },
        );
        // Live INTENT: the account holds its fields. Fake values — the refusal fires
        // PRE-connect, judged purely from the directory, so nothing here ever touches a venue.
        if live_intent {
            policy.with_credentials(
                "bybit",
                &vike_model::accounts::account_keys::AccountLabel::Default,
                vike_model::AccountTier::Demo,
                &[
                    ("API_KEY", "fake-key-for-the-preconnect-gate"),
                    ("API_SECRET", "fake-secret-for-the-preconnect-gate"),
                ],
            )
        } else {
            policy
        }
    };
    let runtime_config = |live_intent: bool| RuntimeConfig {
        backend: crate::local_backend::rig(&HashMap::new(), Some(&policy_of(live_intent)))
            .backend(),
        properties_rec: None,
        seed_cash: 1_000.0,
        reconcile_enabled: false,
        core_config: vike_core::CoreConfig { seed_cash: 1_000.0, ..Default::default() },
        risk_profile: None, // NO operator budget — the thing under test
        // ⚠ bybit's ACCOUNT must be armed, or there is no live intent for the budget rail to
        // refuse: `AdapterPolicy::default()` holds no `account` row, so every venue runs paper, and
        // a paper mount returns the paper engine ABOVE the refusal. This policy holds one active
        // `demo` row for bybit and nothing else, and supplies no budget of its own — so the
        // refusal below is still the budget's.
        policy: policy_of(live_intent),
    };

    let strategy = profile.resolve_strategy(strategies.path(), true).expect("the script compiles");
    // ⚠ An EMPTY preflight report — see `build_live_strategy_core_with_preflight`. `assemble_runtime`
    // would otherwise run the REAL preflight over these fake keys, bybit would refuse the probe,
    // and the enforced demotion would withhold the very credentials that express the LIVE INTENT
    // this test is about — leaving it asserting nothing. It also makes this test network-free,
    // which it was not before: it issued a real signed bybit read whose result merely did not
    // matter yet.
    let no_preflight = vike_tradehub_core::preflight::PreflightReport::default();
    let err = build_live_strategy_core_with_preflight(
        strategy,
        &profile.to_mount_spec(),
        runtime_config(true),
        &no_preflight,
    )
    .err()
    .expect("live intent with no risk budget must refuse the mount");
    let msg = err.to_string();
    assert!(msg.contains("max_notional_per_order"), "names the missing cap: {msg}");
    assert!(msg.contains("max_total_exposure"), "names BOTH missing caps: {msg}");

    // The control: same script, same config, no live intent (bybit's row is `paper`) ⇒ paper mount, no refusal.
    let strategy =
        profile.resolve_strategy(strategies.path(), true).expect("the script compiles again");
    let node = build_live_strategy_core_with_preflight(
        strategy,
        &profile.to_mount_spec(),
        runtime_config(false),
        &no_preflight,
    )
    .expect("no live intent ⇒ the paper account runs, no budget required")
    .runtime;
    assert!(node.live_venues.is_empty(), "no creds ⇒ no live venue");
    assert!(node.handle.is_alive(), "the paper node runs the script strategy");
    node.forwarder_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    node.handle.shutdown_and_join();
}
