//! The Rhai-script twin of `any_strategy_mount.rs`: a profile that names a SCRIPT by path
//! (`[strategy] rhai = "<path>"` — `docs/decisions/0024-rhai-strategies-live.md`) mounts on the
//! SAME production core every named strategy mounts on, and actually trades on the paper exchange.
//!
//! Same shape as the sibling: `main.rs` is a bin, so this exercises the exact library calls `main`
//! composes — `DaemonProfile::from_toml_str` → `resolve_strategy` (which for a script reads the
//! file, hashes it, and compiles `vike_script::RhaiStrategy<LiveBroker>`) →
//! `vike_run::build_paper_strategy_core_with` — then drives CLOSED BARS and asserts a REAL paper
//! fill. The fill's qty is the OVERRIDE the profile's `[strategy.params]` set, which is the
//! end-to-end proof that params flow through `compile_with_params` into the script's own
//! `param("size", …)` — no amount of "it resolved" would show that.
//!
//! The live half of the 0024 rails is here too, network-free on both sides: the pre-connect
//! missing-risk-budget refusal fires for a script mount exactly as for any other (the gate is
//! `vike_mount`'s, strategy-agnostic, and fires on live INTENT before any venue session exists),
//! and the same script over the same `NodeConfig` minus the live intent mounts all-paper — so the
//! refusal is the budget's, not the script's.
//!
//! No network, no creds, no feature flags — default CI lane, like the sibling.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use vike_exec::BarUpdate;
use vike_model::Bar;
use vike_run::{
    build_live_strategy_core_with_preflight, build_paper_strategy_core_with, NodeConfig, PaperHalt,
    PaperMountOpts,
};
use vike_tradehub::config::DaemonProfile;

const SYMBOL: &str = "RHAI_MOUNT_SYMBOL";
const INTERVAL: &str = "1m";

/// A do-something script: buy `size` once from flat, then hold — `buy_hold`'s unambiguous
/// behaviour, spelled in Rhai, with the size as a top-level `param` so the profile override is
/// observable in the fill.
const TRADING_SCRIPT: &str = "const SIZE = param(\"size\", 1.0);\n\
                              fn on_bar() { if position() == 0.0 { buy(SIZE); } }\n";

/// A do-nothing script: the hook exists and does nothing — the minimal mountable strategy.
const DO_NOTHING_SCRIPT: &str = "fn on_bar() {}\n";

/// A HALT sentinel path THIS FILE owns and never creates — the `any_strategy_mount.rs` idiom: a
/// paper mount is HALT-armed by design, so a test expecting fills must pin the sentinel to a path
/// the box's operator cannot have touched.
///
/// ⚠ Returns the owning `TempDir` ALONGSIDE the path, and the caller must BIND it — this is
/// `crates/vike-core/src/scratch.rs`'s `Scratch::reserved` shape: the ROOT exists and is owned,
/// the sentinel INSIDE it does not exist and never will, so "the pinned sentinel must not exist"
/// holds by construction rather than by hoping no earlier run left one at the same pid-keyed path.
///
/// It used to be `env::temp_dir().join(format!("…-{pid}-{name}"))`. Nothing here ever created that
/// directory, so this half leaked nothing — but it inherited the other half of the defect its
/// sibling [`own_script`] hit for real: a pid is REUSED, the CI box runs these tests as two users, and
/// a path built from one is not this test's to assume anything about. The reason and the measured
/// numbers are in `crates/vike-tradehub/src/config.rs`'s `own_script`.
fn own_sentinel(name: &str) -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::Builder::new()
        .prefix(&format!("vike-tradehub-rhai-mount-{name}-"))
        .tempdir()
        .expect("temp sentinel root");
    let path = root.path().join("HALT");
    (root, path)
}

fn opts_pinned_to(sentinel: &Path) -> PaperMountOpts {
    assert!(
        !sentinel.exists(),
        "the pinned sentinel must NOT exist, or the mount refuses opening orders: {}",
        sentinel.display()
    );
    PaperMountOpts { halt: PaperHalt::Pinned(sentinel.to_path_buf()), ..Default::default() }
}

/// A script file this test owns, in a temp directory of its own — the unit tests here never touch
/// a checkout path.
///
/// ⚠ Returns the `TempDir` ALONGSIDE the path, and the caller must BIND it: dropping the guard
/// deletes the script the profile's `rhai = "<path>"` line points at, and the resolve reads that
/// file. Verbatim the shape `crates/vike-tradehub/src/config.rs`'s `own_script` landed on.
///
/// This used to be `env::temp_dir().join(format!("…-{pid}"))` + `create_dir_all`, and the identical
/// helper one crate-file over is the SITE OF A LIVE FLAKE rather than a theoretical one. MEASURED
/// on the CI box, 2026-08-25:
///
/// * **2,171 of these `vike-tradehub-rhai-mount-*` directories were sitting in `/tmp`**, growing
///   by ~13 a day. Nothing ever deleted one.
/// * **A PID is REUSED**, and the CI box runs these tests as two users (`the CI user` for CI, `the operator`
///   for the verification lanes). When a pid collides with a directory the OTHER user made, the
///   `create_dir_all` succeeds (it already exists) and the `fs::write` fails `PermissionDenied`.
///   That is how `crates/vike-tradehub/src/config.rs`'s
///   `a_rhai_profile_resolves_to_a_script_mount_carrying_the_audit_hash` went red on a lane while
///   `main` passed the same lane minutes later — through its own copy of this helper (a different
///   prefix, `vike-tradehub-config-rhai-{pid}`), since repaired in #1528. This file's copy is the
///   same idiom against a different name, which changes nothing about either defect.
///
/// `tempfile::TempDir` fixes both halves at once: unique by construction, and self-deleting even
/// when an assertion unwinds.
fn own_script(name: &str, source: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("temp script dir");
    let path = dir.path().join(format!("{name}.rhai"));
    std::fs::write(&path, source).expect("write script");
    (dir, path)
}

/// TOML-safe spelling of a path (backslashes escaped, for the Windows dev box).
fn toml_path(p: &Path) -> String {
    p.display().to_string().replace('\\', "\\\\")
}

fn bar(ts: i64, px: f64) -> Bar {
    Bar {
        ts,
        open: px,
        high: px,
        low: px,
        close: px,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

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

/// THE headline + the params-flow proof in one: the profile names a script by PATH, the script's
/// `param("size", 1.0)` is overridden to 3.0 by `[strategy.params]`, and the paper fill is for
/// exactly 3.0 — so the override was baked into the compiled scope, not merely accepted.
#[test]
fn a_rhai_profile_mounts_and_trades_with_the_overridden_size() {
    vike_log::test_init();

    let (_script_dir, script) = own_script("trades", TRADING_SCRIPT);
    let profile = DaemonProfile::from_toml_str(&format!(
        r#"
venue = "polymarket"
symbol = "{SYMBOL}"
interval = "{INTERVAL}"
interval_ms = 60000

[strategy]
rhai = "{}"

[strategy.params]
size = 3.0
"#,
        toml_path(&script)
    ))
    .expect("a rhai profile parses and validates");

    let cfg = profile.to_mount_config();
    let strategy = profile.resolve_strategy(&cfg).expect("the script compiles and resolves");

    let (_halt_dir, sentinel) = own_sentinel("trades");
    let mount = build_paper_strategy_core_with(
        strategy,
        &profile.to_mount_spec(),
        opts_pinned_to(&sentinel),
    );

    // Two CLOSED bars: the first reaches `on_bar` (the script buys SIZE from flat), the paper book
    // fills the market order at the SECOND bar's open — the sibling test's discipline.
    let bars = mount.handle.bar_sender();
    for (i, px) in [0.40_f64, 0.50].into_iter().enumerate() {
        bars.close(BarUpdate {
            venue: "polymarket".to_string(),
            symbol: SYMBOL.to_string(),
            interval: INTERVAL.to_string(),
            bar: bar(60_000 * (i as i64 + 1), px),
        })
        .expect("the core is alive");
    }

    let filled = wait_until(10, || !mount.fills.lock().expect("fills").is_empty());
    let fills = mount.fills.lock().expect("fills");
    assert!(filled, "the script never traded: no paper fill after two closed bars");
    let f = &fills[0];
    assert_eq!(
        f.qty.to_bits(),
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

    let (_script_dir, script) = own_script("does-nothing", DO_NOTHING_SCRIPT);
    let profile = DaemonProfile::from_toml_str(&format!(
        "venue = \"polymarket\"\nsymbol = \"{SYMBOL}\"\ninterval = \"{INTERVAL}\"\n\
         interval_ms = 60000\n[strategy]\nrhai = \"{}\"\n",
        toml_path(&script)
    ))
    .expect("a params-free rhai profile parses and validates");

    let cfg = profile.to_mount_config();
    let strategy = profile.resolve_strategy(&cfg).expect("the do-nothing script compiles");
    let (_halt_dir, sentinel) = own_sentinel("does-nothing");
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
            bar: bar(60_000, 0.40),
        })
        .expect("the core is alive");
    std::thread::sleep(Duration::from_millis(200));
    assert!(mount.handle.is_alive(), "still alive after driving the script's on_bar");
    assert!(mount.fills.lock().expect("fills").is_empty(), "a do-nothing script trades nothing");

    mount.handle.shutdown_and_join();
}

/// The 0024 risk-budget rail, proven at the script mount and network-free on both sides:
///
/// * WITH live intent (a bybit demo key pair in the vars map — `would_mount_live`'s own gate) and
///   NO operator risk budget, the live build REFUSES pre-connect, naming the missing caps. The
///   gate is `vike_mount`'s `require_live_risk_budget` — strategy-agnostic, which is exactly the
///   claim: a script cannot slip past it, because the gate never sees what the strategy is.
/// * WITHOUT the live intent (empty vars), the SAME script over the SAME `NodeConfig` mounts
///   all-paper — so the refusal above is the budget's, not the script's.
#[test]
fn a_live_rhai_mount_without_a_risk_budget_is_refused_pre_connect() {
    vike_log::test_init();

    let (_script_dir, script) = own_script("live-budget", DO_NOTHING_SCRIPT);
    // The wired bybit pair (`vike_run::WIRED_MARKETS`), so the refusal under test is the budget's
    // and not a routing one.
    let profile = DaemonProfile::from_toml_str(&format!(
        "venue = \"bybit\"\nsymbol = \"BTCUSDT\"\ninterval = \"{INTERVAL}\"\n\
         interval_ms = 60000\n[strategy]\nrhai = \"{}\"\n",
        toml_path(&script)
    ))
    .expect("a bybit rhai profile parses and validates");
    assert!(
        profile.validate_for_live().is_ok(),
        "bybit/BTCUSDT is a live-wired pair — a rhai profile passes the same gate a named one does"
    );

    let node_cfg = |vars: HashMap<String, String>| NodeConfig {
        vars,
        properties_rec: None,
        seed_cash: 1_000.0,
        recon_enabled: false,
        core_config: vike_core::CoreConfig { seed_cash: 1_000.0, ..Default::default() },
        risk_profile: None, // NO operator budget — the thing under test
        // ⚠ The ARMING CEILING must permit bybit, or there is no live intent for the budget rail to
        // refuse: `MountPolicy::default()` caps every venue at `paper`, and a capped mount returns
        // the paper engine ABOVE the refusal. This policy arms bybit and nothing else, and supplies
        // no budget of its own — so the refusal below is still the budget's.
        policy: vike_run::MountPolicy {
            venues: vike_config::VenuePolicy::default()
                .declare("bybit", vike_config::VenueMode::Demo),
            ..vike_run::MountPolicy::default()
        },
    };

    // Live INTENT: a present demo key pair. Fake values — the refusal fires PRE-connect, judged
    // purely from the vars map, so nothing here ever touches a venue.
    let live_intent: HashMap<String, String> = [
        ("BYBIT_DEMO_API_KEY".to_string(), "fake-key-for-the-preconnect-gate".to_string()),
        ("BYBIT_DEMO_API_SECRET".to_string(), "fake-secret-for-the-preconnect-gate".to_string()),
    ]
    .into();

    let cfg = profile.to_mount_config();
    let strategy = profile.resolve_strategy(&cfg).expect("the script compiles");
    // ⚠ An EMPTY preflight report — see `build_live_strategy_core_with_preflight`. `build_node`
    // would otherwise run the REAL preflight over these fake keys, bybit would refuse the probe,
    // and the enforced demotion would withhold the very credentials that express the LIVE INTENT
    // this test is about — leaving it asserting nothing. It also makes this test network-free,
    // which it was not before: it issued a real signed bybit read whose result merely did not
    // matter yet.
    let no_preflight = vike_run::PreflightReport::default();
    let err = build_live_strategy_core_with_preflight(
        strategy,
        &profile.to_mount_spec(),
        node_cfg(live_intent),
        &no_preflight,
    )
    .err()
    .expect("live intent with no risk budget must refuse the mount");
    let msg = err.to_string();
    assert!(msg.contains("max_notional_per_order"), "names the missing cap: {msg}");
    assert!(msg.contains("max_total_exposure"), "names BOTH missing caps: {msg}");

    // The control: same script, same config, no live intent ⇒ all-paper mount, no refusal.
    let strategy = profile.resolve_strategy(&cfg).expect("the script compiles again");
    let node = build_live_strategy_core_with_preflight(
        strategy,
        &profile.to_mount_spec(),
        node_cfg(HashMap::new()),
        &no_preflight,
    )
    .expect("no live intent ⇒ every venue paper, no budget required")
    .node;
    assert!(node.live_venues.is_empty(), "no creds ⇒ no live venue");
    assert!(node.handle.is_alive(), "the paper node runs the script strategy");
    node.forwarder_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    node.handle.shutdown_and_join();
}
