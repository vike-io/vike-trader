//! THE headline proof: the daemon mounts a strategy the profile NAMED — not the hardcoded A-S maker
//! — and that strategy actually trades.
//!
//! `main.rs` is a bin, so an integration test cannot call it; it exercises the exact three library
//! calls `main` composes, in `main`'s order:
//!
//! 1. `DaemonProfile::from_toml_str` (parse + validate, including the strategy-capability gate),
//! 2. `DaemonProfile::resolve_strategy` (the shared `vike_strategy` registry, at `LiveBroker`),
//! 3. `vike_run::build_paper_strategy_core_with(strategy, &profile.to_mount_spec(), …)`.
//!
//! Then it drives CLOSED BARS onto the mount's own bar lane and asserts a REAL paper fill appears —
//! which no amount of "it resolved" would prove. `buy_hold` is the strategy under test precisely
//! because its behaviour is unambiguous (buy `size` once on the first bar, then hold), so a fill of
//! exactly `size` at the next bar's open is a complete statement about what ran.
//!
//! No network, no creds, no feature flags: the paper exchange is the `ExecutionClient` and the bars
//! are hand-fed, so this runs in the default CI lane.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use vike_exec::{BarUpdate, OrderStatus};
use vike_model::Bar;
use vike_run::{build_paper_strategy_core_with, PaperHalt, PaperMountOpts};
use vike_tradehub::config::DaemonProfile;

const SYMBOL: &str = "ANY_STRATEGY_SYMBOL";
const INTERVAL: &str = "1m";

/// A HALT sentinel path THIS FILE owns and (except in the one test that engages it deliberately)
/// never creates.
///
/// ⚠ **A paper MOUNT is HALT-armed by design** (`vike_run::PaperHalt`), so a test that stands one up
/// and expects fills inherits the OPERATOR's kill switch off whatever box runs it — the mount would
/// refuse `buy_hold`'s opening order and the fill assertions below would fail as a strategy-resolution
/// mystery that never mentions halt. MEASURED on the CI box when this seam was first armed:
/// `VIKE_HALT_FILE=<an existing file> cargo nextest run` over the CI roster turned 22 tests red while
/// the same command without it ran 7147/7147 green. `crates/vike-run/tests/common/mod.rs` carries the
/// full argument; this is the same cure at the daemon's own mount.
///
/// `name` keys the path per test so the one test that DOES create a sentinel cannot disturb a
/// sibling running in the same process.
///
/// ⚠ Returns the owning `TempDir` ALONGSIDE the path, and the caller must BIND it — this is
/// `crates/vike-core/src/scratch.rs`'s `Scratch::reserved` shape: the ROOT exists and is owned,
/// the `HALT` file inside it does not exist, and whatever a test writes there is removed when the
/// guard drops. That makes "the pinned sentinel must NOT exist" true BY CONSTRUCTION, not by hope.
///
/// It used to be `env::temp_dir().join(format!("…-{pid}-{name}")).join("HALT")`, and both halves
/// of the pid defect applied. The one test below that ENGAGES a sentinel created that directory
/// and removed it by hand only on its success path, so a failing run left a `HALT` file behind
/// under a pid-keyed name — and a later run that drew the same pid would then have found its own
/// "must not exist" sentinel already on disk, halting a mount that expects fills. The measured
/// numbers behind the family (44,840 leaked directories on the CI box on 2026-08-25, and the
/// two-users/one-pid `PermissionDenied` flake) are in `crates/vike-tradehub/src/config.rs`'s
/// `own_script`.
fn own_sentinel(name: &str) -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::Builder::new()
        .prefix(&format!("vike-tradehub-any-strategy-{name}-"))
        .tempdir()
        .expect("temp sentinel root");
    let path = root.path().join("HALT");
    (root, path)
}

/// [`PaperMountOpts::default`] with the sentinel pinned to a path this test owns — see
/// [`own_sentinel`]. Everything else is the SHIPPED default, so these tests still exercise the
/// daemon's own mount options.
fn opts_pinned_to(sentinel: &Path) -> PaperMountOpts {
    assert!(
        !sentinel.exists(),
        "the pinned sentinel must NOT exist, or the mount below refuses opening orders for the \
         reason this pinning exists to rule out: {}",
        sentinel.display()
    );
    PaperMountOpts { halt: PaperHalt::Pinned(sentinel.to_path_buf()), ..Default::default() }
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

#[test]
fn a_profile_named_strategy_mounts_and_trades_on_the_paper_exchange() {
    vike_log::test_init();

    // (1) the profile — the SAME `[strategy]` table shape a backtest profile uses.
    let profile = DaemonProfile::from_toml_str(&format!(
        r#"
venue = "polymarket"
symbol = "{SYMBOL}"
interval = "{INTERVAL}"
interval_ms = 60000

[strategy]
name = "buy_hold"

[strategy.params]
size = 3.0
"#
    ))
    .expect("a [strategy] profile parses and validates");

    // (2) resolve through the shared registry, at `vike_core::LiveBroker`.
    let cfg = profile.to_mount_config();
    let strategy = profile.resolve_strategy(&cfg).expect("buy_hold resolves");

    // (3) mount it on the PRODUCTION live core over the paper exchange.
    let (_halt_dir, sentinel) = own_sentinel("mounts-and-trades");
    let mount = build_paper_strategy_core_with(
        strategy,
        &profile.to_mount_spec(),
        opts_pinned_to(&sentinel),
    );

    // Drive two CLOSED bars. The first reaches `on_bar` (the runtime stamps the series symbol onto
    // it, which is why a symbol-inferring strategy routes correctly live); buy_hold submits ONE
    // market order. The paper book fills a market order at the NEXT bar's open — hence bar two.
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
    assert!(
        filled,
        "the profile-named strategy never traded: no paper fill after two closed bars ({} fills)",
        fills.len()
    );
    let f = &fills[0];
    assert_eq!(f.qty.to_bits(), 3.0_f64.to_bits(), "buy_hold bought `size` (3.0), once");
    assert_eq!(f.side, 1, "…on the buy side");
    // Stamped with the SECOND bar's ts — the next-open market discipline the paper book shares with
    // the backtest engine. Asserting the TIMESTAMP rather than the price proves the same thing (the
    // order was submitted from bar ONE's `on_bar` and filled on bar TWO) without depending on the
    // fill-cost model, which is a different subsystem with its own tests.
    assert_eq!(
        f.ts, 120_000,
        "…filled on the SECOND bar, i.e. submitted from the first one's on_bar"
    );
    assert_eq!(fills.len(), 1, "buy_hold buys ONCE and holds — no re-entry on later bars");
    drop(fills);

    mount.handle.shutdown_and_join();
}

/// The BACK-COMPAT half, asserted as behaviour rather than as prose: a profile with NO `[strategy]`
/// table mounts the A-S maker, which — unlike `buy_hold` — trades on the QUOTE/BOOK lane and not on
/// bars. So the identical bar sequence produces NO fill. That single asymmetry is what proves the
/// two profiles really mount different strategies, and that the default is still the maker.
#[test]
fn a_profile_with_no_strategy_table_still_mounts_the_as_maker() {
    vike_log::test_init();

    let profile = DaemonProfile::from_toml_str(&format!(
        "venue = \"polymarket\"\ntoken_id = \"{SYMBOL}\"\ninterval = \"{INTERVAL}\"\ninterval_ms = 60000\n"
    ))
    .expect("the historical profile shape parses");
    assert!(profile.strategy.is_none());

    let cfg = profile.to_mount_config();
    let strategy = profile.resolve_strategy(&cfg).expect("the A-S maker builds");
    let (_halt_dir, sentinel) = own_sentinel("no-strategy-table");
    let mount = build_paper_strategy_core_with(
        strategy,
        &profile.to_mount_spec(),
        opts_pinned_to(&sentinel),
    );

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
    // Give the core the same window the positive test uses, then assert the maker did NOT trade off
    // bars alone — it prices on `on_quote_tick`/`on_order_book`, which nothing drove here.
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        mount.fills.lock().expect("fills").is_empty(),
        "the default mount is the A-S maker, which quotes on ticks — a bar-only run must not fill"
    );

    mount.handle.shutdown_and_join();
}

/// ⚠ **THE KILL SWITCH REACHES A REGISTRY-STRATEGY MOUNT.** The daemon used to mount exactly one
/// strategy; it now mounts whatever the profile named, through a builder that takes the strategy as
/// a parameter — and that parameter must not be able to change whether `touch $VIKE_HALT_FILE`
/// stops the node.
///
/// This is written as an END-TO-END behaviour rather than as another `halt_path()` assertion because
/// the hazard it guards is a MERGE, not a typo. `PaperHalt` and the arming inside
/// `crates/vike-run/src/lib.rs`'s `paper_client_for` landed on `main` while this branch was in
/// flight, in the very lines the branch rewrote; resolving that conflict the other way compiles
/// cleanly, passes every strategy test, and silently disarms the operator's kill switch on the paper
/// daemon that runs on the CI box. So the proof has to be "an order was actually refused", which no
/// resolution can satisfy by accident.
///
/// The evidence is POSITIVE on both sides, which is what makes it a proof rather than a timeout:
/// the order reaches the registry and lands in [`OrderStatus::Rejected`] (so the strategy really did
/// try to trade — an empty `fills` alone would also be what a strategy that never traded looks
/// like), and no fill is ever produced. Sibling
/// `a_profile_named_strategy_mounts_and_trades_on_the_paper_exchange` is the control: the same
/// profile, the same bars and a sentinel that does NOT exist fills for exactly 3.0.
///
/// MUTATION: drop `.with_halt_path(halt.resolve())` from `paper_client_for` — the single line a
/// rebase toward this branch would have deleted — and this goes red on EVERY box, because the
/// sentinel it engages is one the test writes rather than one the machine happened to have.
#[test]
fn the_operator_halt_sentinel_stops_a_registry_strategy_mount() {
    vike_log::test_init();

    // The one test that ENGAGES a sentinel. `own_sentinel`'s root already exists (it is the owned
    // `TempDir`), so there is nothing to `create_dir_all` — and the guard removes the file and the
    // directory at the end of the test, on the failure path too.
    let (_halt_dir, sentinel) = own_sentinel("halt-engaged");
    std::fs::write(&sentinel, b"").expect("engage the sentinel");
    assert!(
        sentinel.exists(),
        "the sentinel must be ON DISK before the mount is built, or this test passes vacuously — \
         the exact defect it exists to prevent"
    );

    // Byte-for-byte the profile the control test mounts.
    let profile = DaemonProfile::from_toml_str(&format!(
        r#"
venue = "polymarket"
symbol = "{SYMBOL}"
interval = "{INTERVAL}"
interval_ms = 60000

[strategy]
name = "buy_hold"

[strategy.params]
size = 3.0
"#
    ))
    .expect("a [strategy] profile parses and validates");
    let cfg = profile.to_mount_config();
    let strategy = profile.resolve_strategy(&cfg).expect("buy_hold resolves");
    let mount = build_paper_strategy_core_with(
        strategy,
        &profile.to_mount_spec(),
        PaperMountOpts { halt: PaperHalt::Pinned(sentinel.clone()), ..Default::default() },
    );

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

    let refused = wait_until(10, || {
        mount.handle.snapshot().orders.iter().any(|o| o.status == OrderStatus::Rejected)
    });
    assert!(
        refused,
        "the halted mount registered no REJECTED order — either the strategy never submitted (so \
         this proves nothing about halt) or the paper book admitted an opening order while the \
         operator sentinel {} existed",
        sentinel.display()
    );
    assert!(
        mount.fills.lock().expect("fills").is_empty(),
        "an order FILLED on a halted mount: `touch`ing the sentinel does not stop this node"
    );

    mount.handle.shutdown_and_join();
    // No hand cleanup: `_halt_dir` drops here and removes the engaged sentinel with its root. The
    // pair of `remove_file`/`remove_dir` calls that used to sit here ran ONLY when the test got
    // this far, so a failing run left an engaged `HALT` behind under a pid-keyed name.
}

/// ⚠ **A params `symbol` that disagrees with the mount is REFUSED — and the refusal guards a real
/// misroute, proven by driving one.**
///
/// MEASURED on the CI box against `89c4838d`: a `buy_hold` profile with mount `symbol = "MOUNTED_SYMBOL"`
/// and `[strategy.params] symbol = "A_COMPLETELY_DIFFERENT_SYMBOL"` LOADED, announced
/// `size=3 symbol=A_COMPLETELY_DIFFERENT_SYMBOL`, and filled on `MOUNTED_SYMBOL`. The startup line
/// named one instrument; the orders hit another. Cause:
/// `crates/vike-core/src/runtime/strategy_drive.rs`'s `resolve_intent_symbol` returns the MOUNT's
/// symbol unconditionally while `any_mount_multi` is false, and it is false on every mount this
/// workspace builds.
///
/// Two halves, and the SECOND is what makes this a proof rather than a spelling test:
///
/// 1. the profile no longer loads, and the refusal names BOTH instruments;
/// 2. the misroute it guards is REAL — the identical strategy, resolved from the identical params
///    and mounted while BYPASSING validation, fills on the MOUNT's symbol. A refusal for a thing
///    that could not happen would be noise; this drives the thing.
///
/// MUTATION: delete the `misrouted_params` block from `DaemonProfile::validate_strategy` and half 1
/// goes red (the profile loads). Half 2 stays green either way — it is the evidence, not the guard.
#[test]
fn a_params_symbol_that_disagrees_with_the_mount_is_refused_and_the_misroute_is_real() {
    vike_log::test_init();

    const OTHER: &str = "A_COMPLETELY_DIFFERENT_SYMBOL";
    let toml = format!(
        r#"
venue = "polymarket"
symbol = "{SYMBOL}"
interval = "{INTERVAL}"
interval_ms = 60000

[strategy]
name = "buy_hold"

[strategy.params]
size = 3.0
symbol = "{OTHER}"
"#
    );

    // --- half 1: the profile is REFUSED, naming both instruments ---
    let err = DaemonProfile::from_toml_str(&toml).err().unwrap_or_else(|| {
        panic!("a params symbol of {OTHER:?} on a mount of {SYMBOL:?} must not load")
    });
    assert!(err.contains(OTHER), "the refusal must name what the profile said: {err}");
    assert!(err.contains(SYMBOL), "…and what the mount actually routes to: {err}");

    // --- half 2: the misroute that refusal prevents, DRIVEN ---
    //
    // The same `[strategy.params]` table, resolved through the same registry call `resolve_strategy`
    // makes, mounted on the same builder — only `DaemonProfile::validate_strategy` is skipped. So
    // this is exactly the mount the refusal now makes unreachable, and it is here to show that it
    // was worth making unreachable.
    let params: toml::Value =
        toml::from_str(&format!("size = 3.0\nsymbol = \"{OTHER}\"\n")).expect("params parse");
    let strategy = vike_strategy::strategy_by_name::<vike_core::LiveBroker>("buy_hold", &params)
        .expect("buy_hold resolves");
    // The mount spec of the profile ABOVE, minus the offending params table — same venue, symbol,
    // interval, so the only difference between this mount and the refused one is the bypass.
    let mounted = DaemonProfile::from_toml_str(&format!(
        "venue = \"polymarket\"\nsymbol = \"{SYMBOL}\"\ninterval = \"{INTERVAL}\"\n\
         interval_ms = 60000\n"
    ))
    .expect("the mount half of the same profile");
    let (_halt_dir, sentinel) = own_sentinel("misroute-is-real");
    let mount = build_paper_strategy_core_with(
        strategy,
        &mounted.to_mount_spec(),
        opts_pinned_to(&sentinel),
    );

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

    let traded = wait_until(10, || !mount.handle.snapshot().orders.is_empty());
    let orders = mount.handle.snapshot().orders.clone();
    assert!(
        traded,
        "the bypassed mount produced NO order, so it demonstrates nothing about routing"
    );
    // THE POINT: every order carries the MOUNT's symbol, never the one the params named — which is
    // precisely why echoing the params value was a false claim and why the profile is now refused.
    assert_ne!(SYMBOL, OTHER, "the two names must differ, or this proves nothing");
    for o in &orders {
        assert_eq!(
            o.symbol, SYMBOL,
            "the core routed an order to {:?}, so the params symbol was NOT overridden and the \
             premise of the refusal is wrong",
            o.symbol
        );
    }

    mount.handle.shutdown_and_join();
}

/// The mount line states ORDER SIZE only where that statement is TRUE.
///
/// ⚠ `qty` is the field an operator scans for order size, and on a registry mount the daemon printed
/// the A-S maker's DEFAULT there — `qty=20.0` beside `strategy=grid` while the grid's real size sat
/// two fields along in `strategy_params`. That is a diagnostic making a positive claim about a knob
/// that configures nothing, the same class `DaemonProfile::effective_params`' own doc records this
/// daemon having shipped and undone once already.
///
/// Two halves, because either alone is satisfiable by the wrong fix:
///
/// 1. **BEHAVIOURAL** — `effective_params` is the ONE authority on what the mounted strategy is
///    configured with, and it must state `qty`/`resolution_ts` on a MAKER mount and the registry
///    strategy's own knobs (with no `qty` anywhere) on a registry mount. Deleting the field from
///    `main.rs` while `effective_params` also stopped reporting it would lose the number entirely;
///    this half refuses that.
/// 2. **TEXT** — NEITHER mount announcement in `main.rs` carries a `qty` / `resolution_ts` field of
///    its own: not the PAPER `tracing::info!`, and not the LIVE `tracing::warn!` that announces the
///    same registry `strategy_name`. `main.rs` is a bin, so no integration test can call it — this is
///    the idiom `crates/vike-ops/tests/graceful_stop_pin.rs` and
///    `crates/vike-ops/tests/kill_switch_gate.rs` already use for that surface. MUTATION: put
///    `qty = cfg.qty,` back in EITHER macro and this goes red — see the loop's own comment for why
///    checking one arm was not enough.
#[test]
fn the_mount_line_states_order_size_only_where_it_is_true() {
    let registry = DaemonProfile::from_toml_str(&format!(
        "venue = \"polymarket\"\nsymbol = \"{SYMBOL}\"\ninterval = \"{INTERVAL}\"\n\
         interval_ms = 60000\n\n[strategy]\nname = \"grid\"\n\n[strategy.params]\nsize = 2.0\n"
    ))
    .expect("a grid profile parses");
    let echo = registry.effective_params(&registry.to_mount_config());
    assert!(echo.contains("size=2"), "the grid's REAL size must be in the echo: {echo}");
    assert!(
        !echo.contains("qty"),
        "a registry mount has no A-S `qty`; naming one anywhere on this line is the defect: {echo}"
    );
    assert!(!echo.contains("resolution_ts"), "…and no A-S resolution horizon either: {echo}");

    // The MAKER mount, where the claim is true: the number still prints, from the one place that
    // knows which strategy was mounted.
    let maker = DaemonProfile::from_toml_str(&format!(
        "venue = \"polymarket\"\ntoken_id = \"{SYMBOL}\"\ninterval = \"{INTERVAL}\"\n\
         interval_ms = 60000\n"
    ))
    .expect("the historical profile shape parses");
    let cfg = maker.to_mount_config();
    let maker_echo = maker.effective_params(&cfg);
    assert!(
        maker_echo.contains(&format!("qty={}", cfg.qty)),
        "the A-S maker's qty must still be reported — deleting the standalone field must not lose \
         the number: {maker_echo}"
    );
    assert!(maker_echo.contains("resolution_ts="), "…and its horizon too: {maker_echo}");

    // --- the TEXT half: the daemon's mount ANNOUNCEMENTS, BOTH of them ---
    //
    // ⚠ **TWO arms announce a mount, and a guard on one cannot see the other.** The PAPER
    // `tracing::info!` and the LIVE `tracing::warn!` beside `validate_for_live` carry the IDENTICAL
    // field prefix (`venue` / `token` / `interval` / `strategy` / `strategy_params`), so an edit made
    // to one by symmetry lands on the other just as easily. Only the PAPER arm ever grew `qty`, and
    // only it is the defect this test was written for; the LIVE arm is checked because it announces
    // the SAME registry `strategy_name` and is one copy-paste from the same lie.
    //
    // MEASURED, and the reason this is a loop rather than the single-arm check it started as: a
    // round-4 mutation that inserted `qty = cfg.qty,` by matching
    // `interval = %cfg.interval,` + `strategy = %strategy_name,` — a two-line anchor that is NOT
    // unique — landed on the LIVE arm, the first match in the file, and this test stayed GREEN while
    // `main.rs` genuinely carried the field. That mutation was invalid rather than this guard being
    // wrong, but a guard a plausible wrong edit walks straight past is worth widening.
    // ⚠ `tradehub_cli.rs`, not `main.rs`, since the multicall merge moved the daemon's body into the
    // library so the `vike` dispatcher could reach it. The mount announcements travelled with it;
    // `main.rs` is now a four-line shim and this anchor would find nothing there.
    let main_rs =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("tradehub_cli.rs");
    let src = std::fs::read_to_string(&main_rs).expect("read the daemon's main.rs");
    // ⚠ Each anchor must be UNIQUE, and that is ASSERTED below rather than assumed. `"the live gate
    // is ON` alone is NOT unique — the `validate_for_live` failure branch a few lines above opens
    // with `tracing::error!("the live gate is ON but the profile is not live-wireable`, so a
    // first-match `find` lands there, `rfind` walks back to some unrelated earlier macro, and the
    // slice becomes a hundred lines of `main.rs` that trivially satisfies (or trivially violates)
    // every assert below. Measured while widening this test: it matched the error branch and the
    // dumped "field list" contained `return ExitCode::FAILURE;`.
    const ANNOUNCEMENTS: [(&str, &str); 2] = [
        ("\"mounting the PAPER strategy", "tracing::info!("),
        ("\"the live gate is ON (flags.toml", "tracing::warn!("),
    ];
    for (message, macro_open) in ANNOUNCEMENTS {
        assert_eq!(
            src.matches(message).count(),
            1,
            "the anchor {message} must match {} EXACTLY once, or this test is inspecting a slice \
             nobody chose — shorten/lengthen it until it does",
            main_rs.display()
        );
        let msg_at = src.find(message).unwrap_or_else(|| {
            panic!("{} no longer carries the mount message {message}", main_rs.display())
        });
        let macro_at = src[..msg_at].rfind(macro_open).unwrap_or_else(|| {
            panic!("the mount message {message} must be emitted by a {macro_open}")
        });
        // Only the FIELD LIST — everything between the macro and its message. Comments above the
        // macro (which discuss `qty` at length, deliberately) are outside this slice by
        // construction; a `//` comment INSIDE the field list is stripped so it cannot spoof a field
        // either.
        let fields: String = src[macro_at + macro_open.len()..msg_at]
            .lines()
            .map(|l| l.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            fields.contains("strategy = ") && fields.contains("strategy_params = "),
            "the mount line {message} must still say WHICH strategy and with what: {fields}"
        );
        for a_s_only in ["qty", "resolution_ts"] {
            assert!(
                !fields.contains(a_s_only),
                "the mount line {message} carries an A-S-only field `{a_s_only}`, which configures \
                 nothing on a registry mount — it belongs in `effective_params`, which knows which \
                 strategy was mounted:\n{fields}"
            );
        }
    }
}
