//! `vike-cli init` — the scaffold is IDEMPOTENT, `--reset` is narrow, and every shipped example
//! actually runs.
//!
//! Two kinds of test live here, and both are about a promise that is easy to make and hard to keep.
//!
//! # 1. The scaffold must never eat somebody's work
//!
//! These drive the REAL binary (`CARGO_BIN_EXE_vike-cli`) against a throwaway directory rather than
//! calling the parser, because the property is about what lands on DISK. `--dir` points every run
//! at a temp directory, so the tests are hermetic and never touch a real project — a scaffolding
//! command whose test suite wrote into the checkout would be its own worst example.
//!
//! # 2. The Rhai examples must RUN, and compiling is not evidence of that
//!
//! Rhai resolves function names at CALL time. A script naming a misspelled indicator — or any name
//! outside `crates/vike-script/src/engine.rs`'s `RHAI_INDICATORS`, the set the host actually
//! registers — compiles perfectly, then errors on every bar and self-disables after the
//! consecutive-error cap. It would look mounted and silently never trade. A shipped example that
//! teaches that is worse than no example, so `every_shipped_rhai_strategy_reaches_the_broker`
//! mounts each one through the real `RhaiStrategy`, drives real bars, and asserts an order arrives.
//!
//! ⚠ Nothing here names an indicator: every assertion about the callable set reads
//! `vike_script::RHAI_INDICATORS`, the same const the host binds from, so no test in this file can
//! pass while the shipped documentation describes a different set.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use vike_cli::cmd::init::content;
use vike_model::strategy::MockBroker;
use vike_model::{Bar, Strategy};
use vike_script::RhaiStrategy;

/// Every file the scaffold ships, as the test's OWN list.
///
/// Deliberately a second copy rather than a read of `init`'s private table: a test that asked the
/// implementation what it ships could only ever agree with itself. This is the list a user was
/// promised, so a file silently dropped from the scaffold fails here.
const EXPECTED: &[&str] = &[
    "README.md",
    "strategies/rhai/README.md",
    "strategies/rhai/sma_cross/sma_cross.rhai",
    "strategies/rhai/sma_cross/fast.toml",
    "strategies/rhai/sma_cross/slow.toml",
    "strategies/rhai/rsi_meanrev/rsi_meanrev.rhai",
    "strategies/rhai/rsi_meanrev/default.toml",
    "strategies/rhai/ema_trend/ema_trend.rhai",
    "strategies/rhai/ema_trend/default.toml",
    "strategies/rust/README.md",
    "strategies/rust/my_experiment/my_experiment.rs",
    "indicators/README.md",
    "indicators/donchian_high.rhai",
    "indicators/streak.rhai",
    "profiles/README.md",
    "profiles/backtest.toml",
    "profiles/sweep.toml",
    "profiles/walkforward.toml",
    "backtest_results/README.md",
    "backtest_results/sample_sma_cross.json",
    "notebooks/README.md",
    "notebooks/backtest_report.ipynb",
    "logs/README.md",
];

/// The entry file of each shipped Rhai strategy, beside its source constant.
const SHIPPED_SCRIPTS: &[(&str, &str)] = &[
    ("sma_cross", content::SMA_CROSS_RHAI),
    ("rsi_meanrev", content::RSI_MEANREV_RHAI),
    ("ema_trend", content::EMA_TREND_RHAI),
];

/// Each shipped user INDICATOR, beside its source constant. Same second-copy discipline as
/// [`EXPECTED`]: asking `init` what it ships could only agree with itself.
const SHIPPED_INDICATORS: &[(&str, &str)] =
    &[("donchian_high", content::DONCHIAN_HIGH_RHAI), ("streak", content::STREAK_RHAI)];

/// Run the shipped binary in an environment that resolves no project settings, so `resolve_policy`
/// (which runs before EVERY subcommand) cannot pick up this machine's real `policy.toml`.
fn run(args: &[&str]) -> Output {
    let empty_settings = std::env::temp_dir().join("vike_cli_init_no_settings");
    Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .args(args)
        .env("VIKE_SETTINGS_DIR", &empty_settings)
        .env_remove("VIKE_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
        .output()
        .unwrap_or_else(|e| panic!("run vike-cli {args:?}: {e}"))
}

/// `vike-cli init --dir <root> [extra]`, asserted to have succeeded.
fn init(root: &Path, extra: &[&str]) -> String {
    let mut args = vec!["init", "--dir", root.to_str().unwrap()];
    args.extend_from_slice(extra);
    let out = run(&args);
    assert!(
        out.status.success(),
        "init {extra:?} must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn read(root: &Path, rel: &str) -> String {
    let path = at(root, rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        .replace("\r\n", "\n")
}

/// Join a `/`-separated relative path, one component at a time — the tests describe paths the way
/// the docs do, and a literal join would make ONE oddly-named component on Windows.
fn at(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(root.to_path_buf(), |p, c| p.join(c))
}

fn scratch(name: &str) -> tempfile::TempDir {
    tempfile::Builder::new().prefix(&format!("vike_init_{name}_")).tempdir().expect("temp dir")
}

// ── 1. the scaffold ──────────────────────────────────────────────────────────────────────────

/// A fresh scaffold creates every promised file, and PRINTS the map — the command's other job. A
/// user who has to go looking for a README to find out what was just created got half a feature.
#[test]
fn a_fresh_scaffold_creates_every_file_and_prints_the_map() {
    let tmp = scratch("fresh");
    let root = tmp.path().join("user_data");
    let stdout = init(&root, &[]);

    for rel in EXPECTED {
        assert!(at(&root, rel).is_file(), "{rel} was not created");
    }
    // The one folder that ships no example still exists — an absent folder makes the feature
    // undiscoverable, which is the whole reason it is created empty rather than on demand.
    assert!(at(&root, "logs").is_dir(), "logs/ must exist even though it ships only a README");

    assert!(stdout.contains(content::MAP), "the printed output must carry the map verbatim");
    assert!(stdout.contains("user_data:"), "the output must name WHERE it scaffolded");
}

/// ⚠ The property this command lives or dies by: **a re-run never overwrites an edited file.**
/// Also covers the file the user ADDED, which no flag in this command may touch — `--reset` works
/// off a fixed table, so a strategy somebody wrote is not reachable by it.
#[test]
fn re_running_leaves_edits_and_new_files_alone() {
    let tmp = scratch("rerun");
    let root = tmp.path().join("user_data");
    init(&root, &[]);

    let edited = "// MY EDIT — this must survive\nfn on_bar() {}\n";
    std::fs::write(at(&root, "strategies/rhai/sma_cross/sma_cross.rhai"), edited).unwrap();
    let mine = at(&root, "strategies/rhai/my_own/my_own.rhai");
    std::fs::create_dir_all(mine.parent().unwrap()).unwrap();
    std::fs::write(&mine, "fn on_bar() { buy(1.0); }\n").unwrap();
    // A shipped file the user DELETED comes back on a plain re-run: absent is not "edited".
    std::fs::remove_file(at(&root, "profiles/backtest.toml")).unwrap();

    let stdout = init(&root, &[]);

    assert_eq!(
        read(&root, "strategies/rhai/sma_cross/sma_cross.rhai"),
        edited,
        "a re-run must not overwrite an edited sample"
    );
    assert!(mine.is_file(), "a file the user added must be untouched");
    assert!(at(&root, "profiles/backtest.toml").is_file(), "a deleted sample is restored");
    assert!(stdout.contains("backtest.toml"), "the report must name what it recreated: {stdout}");
}

/// A re-run that changes nothing says so, rather than printing a wall of lines that all mean
/// "nothing happened".
#[test]
fn a_clean_re_run_reports_nothing_to_do() {
    let tmp = scratch("noop");
    let root = tmp.path().join("user_data");
    init(&root, &[]);
    let stdout = init(&root, &[]);
    assert!(stdout.contains("nothing to do"), "expected a no-op report, got: {stdout}");
}

/// `--reset` restores a sample the user edited — the Freqtrade `create-userdir --reset` behaviour,
/// and the reason the DEFAULT is safe. It is still narrow: a file the user ADDED is not in the
/// shipped table, so no flag here can reach it.
#[test]
fn reset_restores_an_edited_sample_but_not_a_file_you_wrote() {
    let tmp = scratch("reset");
    let root = tmp.path().join("user_data");
    init(&root, &[]);

    std::fs::write(at(&root, "strategies/rhai/sma_cross/sma_cross.rhai"), "// broken\n").unwrap();
    std::fs::remove_file(at(&root, "backtest_results/sample_sma_cross.json")).unwrap();
    let mine = at(&root, "strategies/rhai/my_own/my_own.rhai");
    std::fs::create_dir_all(mine.parent().unwrap()).unwrap();
    std::fs::write(&mine, "fn on_bar() { buy(1.0); }\n").unwrap();

    let stdout = init(&root, &["--reset"]);

    assert_eq!(
        read(&root, "strategies/rhai/sma_cross/sma_cross.rhai"),
        content::SMA_CROSS_RHAI,
        "--reset must restore an edited sample to the shipped bytes"
    );
    assert_eq!(
        read(&root, "backtest_results/sample_sma_cross.json"),
        content::SAMPLE_RESULT_JSON,
        "--reset must restore a DELETED sample too"
    );
    assert_eq!(
        std::fs::read_to_string(&mine).unwrap(),
        "fn on_bar() { buy(1.0); }\n",
        "--reset must never touch a file the user wrote"
    );
    assert!(stdout.contains("restore"), "the report must say what it restored: {stdout}");
}

/// `--dry-run` reports and writes nothing — a command that can overwrite must be able to answer
/// "what would you do?" before it does it.
#[test]
fn dry_run_writes_nothing() {
    let tmp = scratch("dry");
    let root = tmp.path().join("user_data");

    let stdout = init(&root, &["--dry-run"]);
    assert!(stdout.contains("nothing was written"), "the report must say it wrote nothing");
    assert!(!root.exists(), "--dry-run must not create the directory");

    // ...and on an EDITED tree, --reset --dry-run reports the restore without performing it.
    init(&root, &[]);
    std::fs::write(at(&root, "strategies/rhai/ema_trend/ema_trend.rhai"), "// mine\n").unwrap();
    let stdout = init(&root, &["--reset", "--dry-run"]);
    assert!(stdout.contains("ema_trend.rhai"), "must report the file it would restore: {stdout}");
    assert_eq!(
        read(&root, "strategies/rhai/ema_trend/ema_trend.rhai"),
        "// mine\n",
        "--dry-run must not perform the restore it reported"
    );
}

/// The `strategies/rust/` README's FIRST LINE is the one that answers "I dropped a file in and
/// nothing happened". Pinned here because it is load-bearing prose: a `.rs` file on a release
/// install produces no error, no log line and no strategy, and nothing else on the page recovers
/// from that being buried.
#[test]
fn the_rust_readme_leads_with_the_source_checkout_warning() {
    let first_meaningful = content::RUST_README
        .lines()
        .find(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .expect("the rust README must have prose");
    assert!(
        first_meaningful.contains("compile only in a source checkout"),
        "the first line must be the source-checkout warning, was: {first_meaningful}"
    );
    // And the way out has to be on the same page, or the warning is only bad news.
    assert!(content::RUST_README.contains("../rhai/"), "it must point at the alternative");
}

/// ⚠ **A capability somebody expects and does not find must be explained where they look.**
///
/// This test used to assert that the root README explained the ABSENCE of `indicators/`. The folder
/// ships now, so that premise is gone — but the half of the guarantee that survives is the half
/// that was always load-bearing: a `.rs` indicator is still impossible here, and an unexplained
/// "no" is indistinguishable from a broken scaffold.
///
/// It is stated as OUT OF SCOPE rather than deferred, deliberately: `vike-indicators`' registry is a
/// compile-time table under a bit-parity law, so "later" would leave somebody waiting for something
/// that is not coming. And a refusal has to name the thing you CAN do, or it is only bad news.
#[test]
fn the_root_readme_explains_the_indicator_it_still_cannot_ship() {
    let readme = content::readme();
    assert!(readme.contains("`.rs` indicator"), "the impossible half must be named");
    assert!(
        readme.contains("out of scope"),
        "the Rust half is not deferred — saying so is the point"
    );
    assert!(
        readme.contains("Rhai indicator"),
        "it must point at what IS writable, not only at what is refused"
    );
    assert!(
        readme.contains("vike-cli indicators"),
        "...and at the way to see what is already callable"
    );
}

/// ⚠ The user-facing Rhai README must POINT AT the roster instead of reproducing it.
///
/// It used to say "`sma(period)`, `ema(period)`, `rsi(period)`. These three, and no others." — true
/// on the day it was written, and a lie the moment the host widened. A page that names a closed set
/// has to be edited in lockstep with a binding it cannot see; a page that names the COMMAND cannot
/// go stale at all. Non-vacuity: the command it names is asserted to exist, and
/// `crates/vike-cli/tests/indicators_cli.rs` pins that its output is exactly the bound set.
#[test]
fn the_rhai_readme_names_the_roster_command_rather_than_a_closed_set() {
    assert!(
        content::RHAI_README.contains("vike-cli indicators"),
        "the README must tell a user how to SEE the callable set"
    );
    assert!(
        !content::RHAI_README.contains("and no others"),
        "the README must not claim a closed indicator set — it cannot see the binding"
    );
    // The command it points at is real and discoverable from the top-level help.
    let top = run(&["--help"]);
    assert!(top.status.success());
    assert!(
        String::from_utf8_lossy(&top.stdout).contains("indicators"),
        "the README points at `vike-cli indicators`; the top-level help must list it"
    );
}

// ── 2. the examples ──────────────────────────────────────────────────────────────────────────

/// A rising close series: enough bars to clear the slowest shipped warm-up (`slow = 30`) with room
/// to spare. Monotonic on purpose — it puts all three strategies in a decided state (fast above
/// slow, RSI pinned high, close above its EMA), so every one of them has something to do.
fn rising_bars(n: usize) -> Vec<Bar> {
    (0..n)
        .map(|i| {
            let c = 100.0 + i as f64;
            Bar {
                ts: i as i64 * 60_000,
                open: c,
                high: c,
                low: c,
                close: c,
                volume: 1.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: Some("BTCUSDT".into()),
            }
        })
        .collect()
}

/// ⚠ **The gate behind "the shipped examples actually run."** Each script is mounted through the
/// REAL `RhaiStrategy` and driven over real bars; an order must reach the broker.
///
/// Non-vacuous by construction: a script whose indicator name the host does not bind returns `NaN`
/// forever, so its `is_nan()` guard returns early on every bar and NOTHING reaches the broker —
/// which is exactly what this asserts against. The same is true of a hook that errors: the
/// strategy self-disables and submits nothing.
#[test]
fn every_shipped_rhai_strategy_reaches_the_broker() {
    for (name, src) in SHIPPED_SCRIPTS {
        let mut strat = RhaiStrategy::<MockBroker>::compile(src)
            .unwrap_or_else(|e| panic!("{name} must compile: {e}"));
        let mut broker = MockBroker::default();
        for bar in rising_bars(80) {
            broker.px = bar.close;
            strat.on_bar(&mut broker, &bar);
        }
        assert!(
            !broker.markets.is_empty(),
            "{name} placed no order over 80 bars — an example that silently never trades is worse \
             than no example. Check that every indicator it calls is host-bound \
             (`vike_script::RHAI_INDICATORS`)."
        );
        for (symbol, side, qty) in &broker.markets {
            assert_eq!(symbol, "BTCUSDT", "{name} must route to the bar's own symbol");
            assert!(*side == 1 || *side == -1, "{name} sent side {side}, not ±1");
            assert!(*qty > 0.0, "{name} sent a non-positive qty {qty}");
        }
    }
}

/// Every shipped script's knobs are DISCOVERABLE — `param()` is called at the top level, which is
/// where `vike-cli backtest --list-params` and a profile's `[sweep]` grid look for it. A `param()`
/// call moved inside `on_bar` still works at run time and reports nothing here, so the two surfaces
/// that show a user their knobs would show an empty list.
#[test]
fn every_shipped_rhai_strategy_declares_its_knobs_at_the_top_level() {
    for (name, src) in SHIPPED_SCRIPTS {
        let params = vike_script::discover_params(src)
            .unwrap_or_else(|e| panic!("{name} must compile: {e}"));
        assert!(!params.is_empty(), "{name} declares no param() knobs");
        assert!(
            params.iter().any(|(k, _)| k == "qty"),
            "{name} must expose `qty`, the one knob every example shares: {params:?}"
        );
    }
}

/// ⚠ The rule the examples exist to teach: **an indicator is read on every bar, before any branch
/// or early return.** An indicator only advances when the script calls it, so a call inside an `if`
/// silently stops being an average of the last N bars.
///
/// Checked structurally — the first statement of `on_bar` that mentions an indicator must come
/// before the first `if` or `return` in the body. Crude, and deliberately so: the alternative is
/// trusting that whoever edits an example remembers, and this failure is invisible in a result.
///
/// ⚠ The set of names searched for is `vike_script::RHAI_INDICATORS`, not a hard-coded
/// `["sma(", "ema(", "rsi("]`. The hard-coded form was correct only while the host bound exactly
/// those three: an example switched to a fourth indicator would have tripped the
/// "calls no indicator at all" panic with a message describing the opposite of the truth.
#[test]
fn every_shipped_rhai_strategy_reads_its_indicators_before_it_branches() {
    for (name, src) in SHIPPED_SCRIPTS {
        let body =
            src.split("fn on_bar()").nth(1).unwrap_or_else(|| panic!("{name} has no on_bar"));
        // Strip comment lines: the examples EXPLAIN this rule in prose containing both words.
        let code: String = body
            .lines()
            .map(|l| l.split("//").next().unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");
        let first_indicator = vike_script::RHAI_INDICATORS
            .iter()
            .filter_map(|n| code.find(&format!("{n}(")))
            .min()
            .unwrap_or_else(|| panic!("{name}'s on_bar calls no indicator at all"));
        let first_branch =
            ["if ", "return"].iter().filter_map(|n| code.find(n)).min().unwrap_or(usize::MAX);
        assert!(
            first_indicator < first_branch,
            "{name} branches before reading its indicator — the indicator would skip bars and \
             quietly stop being an average of the last N"
        );
    }
}

// The shipped scripts must REACH THE BROKER, not merely compile.
//
// ⚠ This replaced a test that asserted the opposite of the truth. It forbade `abs(` on the belief
// that the host does not register it — and rhai 1.25.1 DOES: `abs(f64)` arrives from
// `packages/arithmetic.rs`'s `f64_functions` via `ArithmeticPackage` -> `CorePackage` ->
// `StandardPackage`, which `vike_script::engine`'s `build_engine` starts from. Three tests that
// already pass drive `abs` over real bars and assert trades happen, so the claim was refutable
// from inside the repo. A merge gate asserting a falsehood does not merely fail to catch bugs; it
// FORBIDS CORRECT CODE, and it shipped that falsehood to users in `RHAI_README`.
//
// The real hazard is the class, not the name: rhai resolves a function name when the line RUNS, so
// an unbound call compiles, raises on every bar, and self-disables after ten failures. Only
// EXECUTION catches that — which is what `every_shipped_rhai_strategy_reaches_the_broker` does.
// This test is deliberately gone rather than rewritten; a name-based check cannot see the class.

/// The shipped profiles are valid TOML with the sections a run needs — the cheapest gate that
/// catches a profile nobody could load, without this DataFusion-free crate linking the engine.
#[test]
fn the_shipped_profiles_are_valid_toml_with_the_sections_a_run_needs() {
    let tmp = scratch("profiles");
    let root = tmp.path().join("user_data");
    init(&root, &[]);

    for rel in ["profiles/backtest.toml", "profiles/sweep.toml", "profiles/walkforward.toml"] {
        let text = read(&root, rel);
        let value: toml::Value =
            toml::from_str(&text).unwrap_or_else(|e| panic!("{rel} must be valid TOML: {e}"));
        for section in ["data", "engine", "strategy"] {
            assert!(value.get(section).is_some(), "{rel} has no [{section}] table");
        }
        assert_eq!(
            value["strategy"]["name"].as_str(),
            Some("rhai"),
            "{rel} must name the rhai strategy"
        );
    }

    // The two profiles with no `--script` flag must carry the script INLINE, byte-identical to the
    // strategy folder's copy — they are written from the same constant, and this is what keeps that
    // true if somebody edits one of them.
    for rel in ["profiles/sweep.toml", "profiles/walkforward.toml"] {
        let value: toml::Value = toml::from_str(&read(&root, rel)).unwrap();
        let src = value["strategy"]["params"]["src"]
            .as_str()
            .unwrap_or_else(|| panic!("{rel} must inline the script as [strategy.params].src"));
        assert_eq!(
            src.trim(),
            content::SMA_CROSS_RHAI.trim(),
            "{rel}'s inlined script must match the one in strategies/rhai/sma_cross/"
        );
    }

    // ...and the sweep must actually grid something, or it is a backtest with extra steps.
    let sweep: toml::Value = toml::from_str(&read(&root, "profiles/sweep.toml")).unwrap();
    assert!(sweep.get("sweep").is_some(), "sweep.toml has no [sweep] grid");
    let wf: toml::Value = toml::from_str(&read(&root, "profiles/walkforward.toml")).unwrap();
    assert!(wf["walkforward"]["n_splits"].as_integer().unwrap() >= 1, "n_splits must be >= 1");
}

/// The sample result parses AND carries the fields the notebook reads — it exists so the notebook
/// runs on a fresh install, which it cannot do if either half is wrong. The notebook itself must be
/// a valid notebook for the same reason.
#[test]
fn the_sample_result_and_the_notebook_are_valid_json() {
    let report: serde_json::Value =
        serde_json::from_str(content::SAMPLE_RESULT_JSON).expect("the sample result must be JSON");
    // Exactly the keys `notebooks/backtest_report.ipynb` reads.
    for field in [
        "name",
        "final_equity",
        "total_return",
        "n_trades",
        "win_rate",
        "sharpe",
        "max_drawdown",
        "profit_factor",
    ] {
        assert!(report.get(field).is_some(), "the sample result has no `{field}`");
    }
    // ⚠ It is illustrative data, and it must SAY so where somebody reading a chart would see it.
    assert!(
        report["name"].as_str().unwrap_or_default().contains("illustrative"),
        "the sample must label itself as illustrative, not pass as a recorded run"
    );

    let nb: serde_json::Value =
        serde_json::from_str(content::NOTEBOOK_IPYNB).expect("the notebook must be JSON");
    assert_eq!(nb["nbformat"].as_i64(), Some(4), "must be nbformat 4");
    assert!(nb["cells"].as_array().is_some_and(|c| !c.is_empty()), "the notebook has no cells");
    assert!(
        content::NOTEBOOK_IPYNB.contains("sample_sma_cross.json"),
        "the notebook must read the sample that ships beside it"
    );
}

/// `--help` is a success on stdout, like every other subcommand, and the top-level help lists the
/// command — a scaffolding command nobody can find is the same as no scaffolding command.
#[test]
fn help_is_clean_and_the_command_is_advertised() {
    let out = run(&["init", "--help"]);
    assert!(out.status.success(), "`init --help` must exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("usage:"), "help must go to stdout");
    assert!(stdout.contains("--reset"), "help must document --reset");
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("help requested"),
        "the internal short-circuit token must never reach a user"
    );

    let top = run(&["--help"]);
    assert!(
        String::from_utf8_lossy(&top.stdout).contains("init"),
        "the top-level help must list `init`"
    );
}

/// ⚠ **The indicator twin of `every_shipped_rhai_strategy_reaches_the_broker`.** Each shipped
/// `indicators/*.rhai` is compiled through the REAL `vike_script::compile_indicator` and streamed
/// over real bars; it must produce a FINITE value, and it must have stopped warming up by the time
/// its own `warmup()` says it has.
///
/// Non-vacuous by construction, and this is the whole point: a broken indicator does not fail — it
/// returns NaN forever, which is indistinguishable from warming up, so a user copying the example
/// would get a strategy that silently never trades. Compiling proves nothing here (rhai resolves a
/// name when the line RUNS), and neither does one bar.
#[test]
fn every_shipped_indicator_computes_a_finite_value_by_its_own_warmup() {
    use vike_script::Indicator;

    for (name, src) in SHIPPED_INDICATORS {
        let mut ind = vike_script::compile_indicator(name, src)
            .unwrap_or_else(|e| panic!("{name} must compile: {e}"));
        let warm = ind.lookback();
        let bars = rising_bars(80);
        assert!(warm < bars.len(), "{name} declares a warm-up of {warm}, longer than this test");

        let out: Vec<f64> = bars.iter().map(|b| ind.on_bar(b)[0]).collect();
        assert!(
            out[warm].is_finite(),
            "{name} is still NaN at bar {warm}, its own declared warm-up — a user copying this \
             example would get a strategy that silently never trades. Fault: {:?}",
            ind.fault()
        );
        assert!(ind.fault().is_none(), "{name} faulted: {:?}", ind.fault());
        assert!(
            out.iter().skip(warm).all(|v| v.is_finite()),
            "{name} goes back to NaN after warming up, which no caller can distinguish from a \
             fault: {out:?}"
        );
    }
}

/// A shipped indicator must be CALLABLE under the name its filename gives it — the load path
/// refuses a name that shadows a built-in or a host verb, so an example that tripped that rule
/// would scaffold and then never load.
#[test]
fn every_shipped_indicator_has_a_name_the_loader_accepts() {
    for (name, _) in SHIPPED_INDICATORS {
        assert!(
            vike_script::user_indicator_conflict(name).is_none(),
            "{name} would be refused at load: {:?}",
            vike_script::user_indicator_conflict(name)
        );
    }
}

/// The README teaches `this`-carried state as the reason the folder exists, so an example that
/// carried none would contradict the document beside it — and would also be expressible as a plain
/// function in a strategy, making the folder pointless.
#[test]
fn every_shipped_indicator_actually_keeps_state_between_bars() {
    for (name, src) in SHIPPED_INDICATORS {
        assert!(
            src.contains("this."),
            "{name} keeps no state, so it demonstrates nothing this folder is for"
        );
    }
}

/// The shipped `donchian_high` example declares a `param()` knob, and that knob must be REACHABLE
/// from a call site — the README beside it now teaches `donchian_high(50)`, so an example whose
/// parameter was decorative would make the document wrong.
#[test]
fn a_shipped_indicators_declared_knob_is_reachable_from_a_call_site() {
    use vike_script::Indicator;

    let ind = vike_script::compile_indicator("donchian_high", content::DONCHIAN_HIGH_RHAI)
        .expect("compiles");
    assert!(!ind.params().is_empty(), "the example must declare a knob the README can teach");

    // The knob must MOVE something observable. Warm-up is the cheapest such witness: a wider window
    // warms later, and an implementation that recorded the argument without applying it would
    // report the same warm-up for both.
    let default_warm = ind.lookback();
    let wider =
        vike_script::compile_indicator_with("donchian_high", content::DONCHIAN_HIGH_RHAI, &[50.0])
            .expect("compiles with an argument");
    assert!(
        wider.lookback() > default_warm,
        "a wider lookback must warm later ({} vs {default_warm}) — otherwise the argument did \
         nothing",
        wider.lookback()
    );
}

/// The two shipped indicators demonstrate BOTH answers to `fn overlay()`, and each is the right one
/// for what it computes: `donchian_high` is a PRICE and declares the hook, `streak` is a bar count
/// and takes the default (its own pane).
///
/// Non-vacuous in both directions — a hard-coded `true` fails on `streak` and a hard-coded `false`
/// fails on `donchian_high` — and that is the point: the scaffold is the one file a new user reads,
/// its comment invites them to DELETE the hook to see the difference, and the README beside it
/// teaches the same three lines. An edit that dropped the hook, or a compile that stopped reading
/// it, would leave all three documents describing a feature the shipped example no longer shows,
/// with nothing anywhere to notice.
#[test]
fn the_shipped_indicators_demonstrate_both_chart_placements() {
    let price = vike_script::compile_indicator("donchian_high", content::DONCHIAN_HIGH_RHAI)
        .expect("compiles");
    assert!(
        price.is_overlay(),
        "donchian_high is a price — it declares `fn overlay() {{ true }}` and must draw over the \
         candles"
    );

    let counter = vike_script::compile_indicator("streak", content::STREAK_RHAI).expect("compiles");
    assert!(
        !counter.is_overlay(),
        "streak is a bar count — it declares no `overlay()`, and the safe default is its own pane"
    );
}
