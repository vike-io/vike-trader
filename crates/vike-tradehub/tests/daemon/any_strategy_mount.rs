//! THE headline proof: the daemon mounts the strategy FILE the profile named — no strategy is compiled
//! into the program — and that strategy actually trades.
//!
//! `main.rs` is a bin, so an integration test cannot call it; it exercises the exact three library
//! calls `main` composes, in `main`'s order:
//!
//! 1. `ProfileDaemon::from_toml_str` (parse + validate, including the strategy-file selection),
//! 2. `ProfileDaemon::resolve_strategy` (read the file from the strategies folder, its header, its
//!    params checks, then compile it at `LiveBroker`),
//! 3. `vike_tradehub_core::build_paper_strategy_core_with(strategy, &profile.to_mount_spec(), …)`.
//!
//! Then it drives CLOSED BARS onto the mount's own bar lane and asserts a REAL paper fill appears —
//! which no amount of "it resolved" would prove. `buy_hold.rhai` (`crate::strategy_files::BUY_HOLD`)
//! is the strategy under test precisely because its behaviour is unambiguous (buy `size` from flat on
//! a bar, then hold), so a fill of exactly `size` at the next bar's open is a complete statement about
//! what ran.
//!
//! No network, no creds, no feature flags: the paper exchange is the `ExecutionClient` and the bars
//! are hand-fed, so this runs in the default CI lane.

use std::time::Duration;

use vike_model::OrderStatus;
use vike_strategies_check::makers::spread_maker::SpreadMaker;
use vike_tradehub::config::{ProfileDaemon, SandboxOpen, SandboxOpened};
use vike_tradehub_core::{
    StrategyCorePaperConfig, StrategyCorePaperHalt, build_paper_strategy_core_with,
};

use crate::strategy_files;
use crate::support::{drive_two_bars, opts_pinned_to, own_sentinel};
use vike_log::poll::wait_until;

const SYMBOL: &str = "ANY_STRATEGY_SYMBOL";
const INTERVAL: &str = "1m";

/// The profile the trading tests mount: `buy_hold.rhai` at `size = 3.0`.
fn buy_hold_profile() -> ProfileDaemon {
    ProfileDaemon::from_toml_str(&format!(
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
    .expect("a [strategy] profile parses and validates")
}

#[test]
fn a_profile_named_strategy_mounts_and_trades_on_the_paper_exchange() {
    vike_log::test_init();

    // (1) the profile — the SAME `[strategy] file` key a backtest profile uses.
    let profile = buy_hold_profile();

    // (2) resolve the file from the strategies folder, at `vike_core::LiveBroker`.
    let strategies = strategy_files::standard();
    let strategy = profile.resolve_strategy(strategies.path(), false).expect("buy_hold resolves");

    // (3) mount it on the PRODUCTION live core over the paper exchange.
    let (_halt_dir, sentinel) = own_sentinel("any-strategy", "mounts-and-trades");
    let mount = build_paper_strategy_core_with(
        strategy,
        &profile.to_mount_spec(),
        opts_pinned_to(&sentinel),
    );

    // Drive two CLOSED bars. The first reaches `on_bar` (the runtime stamps the series symbol onto
    // it, which is why a symbol-inferring strategy routes correctly live); buy_hold submits ONE
    // market order. The paper book fills a market order at the NEXT bar's open — hence bar two.
    drive_two_bars(&mount.handle, "polymarket", SYMBOL);

    let filled =
        wait_until(Duration::from_secs(10), || !mount.fills.lock().expect("fills").is_empty());
    let fills = mount.fills.lock().expect("fills");
    assert!(
        filled,
        "the profile-named strategy never traded: no paper fill after two closed bars ({} fills)",
        fills.len()
    );
    let f = &fills[0];
    assert_eq!(f.size.to_bits(), 3.0_f64.to_bits(), "buy_hold bought `size` (3.0), once");
    assert_eq!(f.side, 1, "…on the buy side");
    // Stamped with the SECOND bar's ts — the next-open market discipline the paper book shares with
    // the backtest engine. Asserting the TIMESTAMP rather than the price proves the same thing (the
    // order was submitted from bar ONE's `on_bar` and filled on bar TWO) without depending on the
    // fill-cost model, which is a different subsystem with its own tests.
    assert_eq!(
        f.timestamp, 120_000,
        "…filled on the SECOND bar, i.e. submitted from the first one's on_bar"
    );
    assert_eq!(fills.len(), 1, "buy_hold buys ONCE and holds — no re-entry on later bars");
    drop(fills);

    mount.handle.shutdown_and_join();
}

/// **A profile with NO strategy file is REFUSED at load, naming the folder** — there is no default
/// strategy to fall back to (no strategy is compiled into the program). The historical profile shape
/// (`token_id` and nothing else) used to mount the A-S maker; it now says what to write instead.
#[test]
fn a_profile_with_no_strategy_file_is_refused_naming_the_folder() {
    let err = ProfileDaemon::from_toml_str(&format!(
        "venue = \"polymarket\"\ntoken_id = \"{SYMBOL}\"\ninterval = \"{INTERVAL}\"\ninterval_ms = 60000\n"
    ))
    .expect_err("a mount without a file must not load");
    assert!(err.contains("names no strategy file"), "says what is missing: {err}");
    assert!(err.contains("`strategies/` folder"), "names the folder a file is read from: {err}");
    assert!(err.contains("[strategy] file = "), "spells the key to write: {err}");
}

/// ⚠ **THE KILL SWITCH REACHES A STRATEGY-FILE MOUNT.** The daemon used to mount exactly one
/// strategy; it now mounts whatever file the profile named, through a builder that takes the strategy
/// as a parameter — and that parameter must not be able to change whether `touch
/// <project>/settings/state/HALT` stops the node.
///
/// This is written as an END-TO-END behaviour rather than as another `halt_path()` assertion because
/// the hazard it guards is a MERGE, not a typo. `StrategyCorePaperHalt` and the arming inside
/// `crates/vike-tradehub-core/src/run/paper.rs`'s `paper_client_for` landed on `main` while this branch was in
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
fn the_operator_halt_sentinel_stops_a_strategy_file_mount() {
    vike_log::test_init();

    // The one test that ENGAGES a sentinel. `own_sentinel`'s root already exists (it is the owned
    // `TempDir`), so there is nothing to `create_dir_all` — and the guard removes the file and the
    // directory at the end of the test, on the failure path too.
    let (_halt_dir, sentinel) = own_sentinel("any-strategy", "halt-engaged");
    std::fs::write(&sentinel, b"").expect("engage the sentinel");
    assert!(
        sentinel.exists(),
        "the sentinel must be ON DISK before the mount is built, or this test passes vacuously — \
         the exact defect it exists to prevent"
    );

    // Byte-for-byte the profile the control test mounts, from the same file.
    let profile = buy_hold_profile();
    let strategies = strategy_files::standard();
    let strategy = profile.resolve_strategy(strategies.path(), false).expect("buy_hold resolves");
    let mount = build_paper_strategy_core_with(
        strategy,
        &profile.to_mount_spec(),
        StrategyCorePaperConfig {
            halt: StrategyCorePaperHalt::Pinned(sentinel.clone()),
            ..Default::default()
        },
    );

    drive_two_bars(&mount.handle, "polymarket", SYMBOL);

    let refused = wait_until(Duration::from_secs(10), || {
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
/// named one instrument; the orders hit another. Cause, then:
/// `crates/vike-core/src/runtime/strategy_drive/broker_drain.rs`'s `resolve_intent_symbol` returned the
/// MOUNT's symbol whatever an order named while `any_mount_multi` is false, which it is on every mount
/// this workspace builds. Since the order language's `on` (step 11) the drain REFUSES such an order
/// instead, so the misroute is closed twice.
///
/// Two halves:
///
/// 1. the mount is refused before it opens, and the refusal names BOTH instruments: a `.rs` file
///    whose header declares `symbol` as a `route = "symbol"` key (today's `buy_hold` registry row; a
///    `.rhai` file can take no text key at all), resolved with an opener that must never be reached;
/// 2. the second line holds — the identical strategy, resolved from the identical params and mounted
///    while BYPASSING validation, trades NOTHING on the mount's symbol: the drain refuses each of its
///    orders by name, naming both instruments, on the operator's ring.
///
/// MUTATION: delete the `misrouted_params` block from `ProfileDaemon::check_params` and half 1 goes
/// red (the opener is reached); make `resolve_intent_symbol` rewrite again and half 2 goes red.
#[test]
fn a_params_symbol_that_disagrees_with_the_mount_is_refused_and_the_misroute_is_real() {
    vike_log::test_init();

    const OTHER: &str = "A_COMPLETELY_DIFFERENT_SYMBOL";
    // `buy_hold`'s registry row, as a header: `size`, and `symbol` naming WHERE an order goes.
    const BUY_HOLD_RS: &str = "\
//! ```toml
//! [strategy]
//! runs_on = \"live\"
//! why = \"a test file: one market buy\"
//!
//! [params.size]
//! type = \"number\"
//! [params.symbol]
//! type = \"text\"
//! route = \"symbol\"
//! ```
";
    let strategies = strategy_files::folder(&[("buy_hold.rs", BUY_HOLD_RS)]);
    let profile = ProfileDaemon::from_toml_str(&format!(
        r#"
venue = "polymarket"
symbol = "{SYMBOL}"
interval = "{INTERVAL}"
interval_ms = 60000

[strategy]
file = "buy_hold.rs"

[strategy.params]
size = 3.0
symbol = "{OTHER}"
"#
    ))
    .expect("the load is pure over the profile: the file's header is judged at resolve");

    // --- half 1: the mount is REFUSED before its sandboxed process opens, naming both instruments ---
    let never_opened = |_: &SandboxOpen<'_>| -> Result<SandboxOpened, String> {
        panic!("a misrouted params table must be refused before the file's process opens")
    };
    let err =
        profile.resolve_mount_with(strategies.path(), false, &never_opened).err().unwrap_or_else(
            || panic!("a params symbol of {OTHER:?} on a mount of {SYMBOL:?} must not mount"),
        );
    assert!(err.contains(OTHER), "the refusal must name what the profile said: {err}");
    assert!(err.contains(SYMBOL), "…and what the mount actually routes to: {err}");

    // --- half 2: the misroute that refusal prevents, DRIVEN, and refused again at the drain ---
    //
    // The same `[strategy.params]` table, given to the same `buy_hold` code (the shipped file's,
    // compiled in-process by `vike_strategies_check::takers`), mounted on the same builder — only
    // `ProfileDaemon::check_params` is skipped. So this is exactly the mount the refusal now makes
    // unreachable, and it shows the drain holds too.
    let params: vike_model::params::Params =
        toml::from_str(&format!("size = 3.0\nsymbol = \"{OTHER}\"\n")).expect("params parse");
    let strategy = vike_strategies_check::takers::buy_hold::build::<vike_core::LiveBroker>(&params);
    // The mount spec of the profile ABOVE — same venue, symbol, interval, so the only difference
    // between this mount and the refused one is the bypass.
    let (_halt_dir, sentinel) = own_sentinel("any-strategy", "misroute-is-real");
    let mount = build_paper_strategy_core_with(
        strategy,
        &profile.to_mount_spec(),
        opts_pinned_to(&sentinel),
    );

    drive_two_bars(&mount.handle, "polymarket", SYMBOL);

    assert_ne!(SYMBOL, OTHER, "the two names must differ, or this proves nothing");
    let denied = |lines: &[std::sync::Arc<str>]| {
        lines.iter().any(|l| l.contains("OrderDenied") && l.contains(OTHER) && l.contains(SYMBOL))
    };
    // THE POINT: the strategy asked for OTHER, and the drain refused it by name rather than trade
    // the mount's SYMBOL in its place, which is what it used to do.
    let refused =
        wait_until(Duration::from_secs(10), || denied(&mount.handle.snapshot().recent_events));
    let snap = mount.handle.snapshot();
    assert!(refused, "the order on {OTHER} must be refused by name: {:?}", snap.recent_events);
    assert!(snap.orders.is_empty(), "nothing traded {SYMBOL} in its place: {:?}", snap.orders);

    mount.handle.shutdown_and_join();
}

/// The mount line states ORDER SIZE only where that statement is TRUE.
///
/// ⚠ `qty` is the field an operator scans for order size, and on a registry mount the daemon printed
/// the A-S maker's DEFAULT there — `qty=20.0` beside `strategy=grid` while the grid's real size sat
/// two fields along in `strategy_params`. That is a diagnostic making a positive claim about a knob
/// that configures nothing, the same class `MountResolvedStrategy::effective_params`' own doc records this
/// daemon having shipped and undone once already.
///
/// Two halves, because either alone is satisfiable by the wrong fix:
///
/// 1. **BEHAVIOURAL** — the resolved mount's `effective_params` is the ONE authority on what the
///    mounted strategy is configured with: a file's own knobs (with no `qty` anywhere) on a mount
///    whose strategy has none, and a `.rs` file's own `echo` rows VERBATIM — the maker's
///    `qty`/`resolution_ts` among them — where the claim is true. Deleting the field from the
///    announcement while the echo also stopped carrying it would lose the number entirely; this half
///    refuses that.
/// 2. **TEXT** — NEITHER mount announcement carries a `qty` / `resolution_ts` field of its own: not
///    the PAPER `tracing::info!`, and not the LIVE `tracing::warn!` that announces the same
///    `strategy_name`. `main.rs` is a bin, so no integration test can call it — this is the idiom
///    `crates/vike-ops/tests/container_deploy/graceful_stop_pin.rs` and
///    `crates/vike-ops/tests/docs/kill_switch_gate.rs` already use for that surface. MUTATION: put
///    `qty = config.qty,` back in EITHER macro and this goes red — see the loop's own comment for why
///    checking one arm was not enough.
#[test]
fn the_mount_line_states_order_size_only_where_it_is_true() {
    let strategies = strategy_files::standard();
    let sized = ProfileDaemon::from_toml_str(&format!(
        "venue = \"polymarket\"\nsymbol = \"{SYMBOL}\"\ninterval = \"{INTERVAL}\"\n\
         interval_ms = 60000\n\n[strategy]\nfile = \"buy_hold.rhai\"\n\n[strategy.params]\n\
         size = 2.0\n"
    ))
    .expect("a buy_hold profile parses");
    let mounted = sized.resolve_mount(strategies.path(), false).expect("buy_hold resolves");
    let echo = mounted.effective_params();
    assert!(echo.contains("size=2"), "the file's REAL size must be in the echo: {echo}");
    assert!(
        !echo.contains("qty"),
        "a mount whose strategy has no `qty` must not name one anywhere on this line: {echo}"
    );
    assert!(!echo.contains("resolution_ts"), "…and no A-S resolution horizon either: {echo}");

    // The MAKER mount, where the claim is true: a `.rs` file's own echo rows (here a stand-in for
    // its sandboxed process's `Opened` answer) print verbatim, from the one place that knows which
    // strategy was mounted.
    let maker_strategies = strategy_files::folder(&[(
        "spread_maker.rs",
        "//! ```toml\n//! [strategy]\n//! runs_on = \"live\"\n//! why = \"a test file\"\n//! ```\n",
    )]);
    let maker = ProfileDaemon::from_toml_str(&format!(
        "venue = \"polymarket\"\ntoken_id = \"{SYMBOL}\"\ninterval = \"{INTERVAL}\"\n\
         interval_ms = 60000\n\n[strategy]\nfile = \"spread_maker.rs\"\n"
    ))
    .expect("a maker profile parses");
    let opened = |_: &SandboxOpen<'_>| -> Result<SandboxOpened, String> {
        Ok(SandboxOpened {
            strategy: Box::new(SpreadMaker::new(20.0, 0.5)),
            echo: vec![
                ("qty".to_string(), "20".to_string()),
                ("resolution_ts".to_string(), "None".to_string()),
            ],
            remote: None,
        })
    };
    let maker_echo = maker
        .resolve_mount_with(maker_strategies.path(), false, &opened)
        .expect("the stand-in opens the maker file")
        .effective_params()
        .to_string();
    assert!(
        maker_echo.contains("qty=20"),
        "the maker's qty must still be reported — deleting the standalone field must not lose the \
         number: {maker_echo}"
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
    // round-4 mutation that inserted `qty = config.qty,` by matching
    // `interval = %config.interval,` + `strategy = %strategy_name,` — a two-line anchor that is NOT
    // unique — landed on the LIVE arm, the first match in the file, and this test stayed GREEN while
    // `main.rs` genuinely carried the field. That mutation was invalid rather than this guard being
    // wrong, but a guard a plausible wrong edit walks straight past is worth widening.
    // ⚠ `tradehub_cli.rs`, not `main.rs`, since the multicall merge moved the daemon's body into the
    // library so the multicall dispatcher (`vike`, `vike-backend` since #1729) could reach it. The
    // mount announcements travelled with it; `main.rs` is now a four-line shim and this anchor
    // would find nothing there. Since the layout split each anchor names its FILE: the PAPER line
    // moved with `announce_paper_start` into `tradehub_cli/paper.rs`, the LIVE one stays in `run`.
    // ⚠ Each anchor must be UNIQUE, and that is ASSERTED below rather than assumed. `"the live gate
    // is ON` alone is NOT unique — the `validate_for_live` failure branch a few lines above opens
    // with `tracing::error!("the live gate is ON but the profile is not live-wireable`, so a
    // first-match `find` lands there, `rfind` walks back to some unrelated earlier macro, and the
    // slice becomes a hundred lines of `main.rs` that trivially satisfies (or trivially violates)
    // every assert below. Measured while widening this test: it matched the error branch and the
    // dumped "field list" contained `return ExitCode::FAILURE;`.
    const ANNOUNCEMENTS: [(&str, &str, &str); 2] = [
        ("tradehub_cli/paper.rs", "\"mounting the PAPER strategy", "tracing::info!("),
        (
            "tradehub_cli.rs",
            "\"the live gate is ON (the `flags.tradehub_live` row)",
            "tracing::warn!(",
        ),
    ];
    for (file, message, macro_open) in ANNOUNCEMENTS {
        let main_rs = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(file);
        let src = std::fs::read_to_string(&main_rs).expect("read the daemon's mount announcement");
        assert_eq!(
            src.matches(message).count(),
            1,
            "the anchor {message} must match {} EXACTLY once, or this test is inspecting a slice \
             nobody chose — shorten/lengthen it until it does",
            main_rs.display()
        );
        let message_at = src.find(message).unwrap_or_else(|| {
            panic!("{} no longer carries the mount message {message}", main_rs.display())
        });
        let macro_at = src[..message_at].rfind(macro_open).unwrap_or_else(|| {
            panic!("the mount message {message} must be emitted by a {macro_open}")
        });
        // Only the FIELD LIST — everything between the macro and its message. Comments above the
        // macro (which discuss `qty` at length, deliberately) are outside this slice by
        // construction; a `//` comment INSIDE the field list is stripped so it cannot spoof a field
        // either.
        let fields: String = src[macro_at + macro_open.len()..message_at]
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
