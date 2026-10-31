//! The `backtest` plane's command lines, pinned as ACCEPTED (decision 0155).
//!
//! Written against the hand-rolled `Flags` parsers and run UNCHANGED against the `clap` ones: each
//! line below is one an operator, a runbook (`docs/ops/backtesthub.md`), the README or a verb's
//! own doc writes today, and the only question asked is whether the argument reader let it through
//! (`common::accepted` explains the sandbox and why "not exit 2" is that question). The lines that
//! are REFUSED on purpose — a retired spelling, a flag with no value — are pinned as refused so a
//! rewrite keeps refusing them.

mod common;

use common::accepted::{assert_accepted, pin, pin_code};

/// `backtest run`'s profile-building flags, each once, in the spelling the docs use.
#[test]
fn run_builds_a_profile_from_flags() {
    assert_accepted(&[
        pin(&["backtest", "run", "--profile", "run.toml"], "docs/ops/backtesthub.md"),
        pin(
            &["backtest", "run", "--profile", "run.toml", "--addr", "127.0.0.1:7880"],
            "docs/ops/backtesthub.md: --addr HOST:PORT",
        ),
        pin(
            &["backtest", "run", "--profile", "run.toml", "--addr=127.0.0.1:7880"],
            "the inline spelling of --addr",
        ),
        pin(
            &[
                "backtest",
                "run",
                "--venue",
                "binance",
                "--symbol",
                "BTCUSDT",
                "--interval",
                "1d",
                "--from",
                "2026-01-01T00",
                "--to",
                "2026-02-01T00",
                "--strategy",
                "buy_hold",
                "--cash",
                "10000",
                "--fee",
                "0.001",
            ],
            "cmd/backtest.rs module doc: the no-file run",
        ),
        pin(
            &[
                "backtest",
                "run",
                "--venue=binance",
                "--symbol=BTCUSDT",
                "--interval=1h",
                "--from=2026-01-01",
                "--to=2026-02-01",
                "--strategy=buy_hold",
                "--cash=10000",
                "--fee=0.001",
                "--slippage=0.0005",
            ],
            "every sugar flag in its inline spelling",
        ),
        pin(
            &[
                "backtest",
                "run",
                "--symbol",
                "BTCUSDT,ETHUSDT",
                "--venue",
                "binance",
                "--cash",
                "1",
            ],
            "--symbol is comma-splitting",
        ),
        pin(
            &["backtest", "run", "--symbol", "BTCUSDT", "--symbol", "ETHUSDT", "--cash", "1"],
            "--symbol is repeatable",
        ),
        pin(&["backtest", "run", "--kind", "tick", "--venue", "binance", "--cash", "1"], "--kind"),
        pin(
            &["backtest", "run", "--decide", "simultaneous", "--venue", "binance", "--cash", "1"],
            "--decide",
        ),
        pin(
            &["backtest", "run", "--profile", "run.toml", "--param", "size=1.5", "--param", "n=2"],
            "--param is repeatable",
        ),
        pin(
            &["backtest", "run", "--profile", "run.toml", "--param", "shift=-3"],
            "a negative number inside a key=value",
        ),
        pin(
            &["backtest", "run", "--profile", "run.toml", "--slippage", "-0.001"],
            "a negative number as the value of a flag",
        ),
        pin(
            &["backtest", "run", "--profile", "run.toml", "--set", "engine.fee_rate=0.001"],
            "--set key=value",
        ),
        pin(
            &["backtest", "run", "--profile", "run.toml", "--set=engine.fee_rate=0.001"],
            "--set, inline spelling, the value keeps its own '='",
        ),
        pin(
            &[
                "backtest",
                "run",
                "--profile",
                "run.toml",
                "--set",
                "engine.slippage=0.1",
                "--set",
                "engine.fee_rate=0.002",
            ],
            "--set is repeatable",
        ),
        pin(&["backtest", "run", "--profile", "run.toml", "--preset", "fast.toml"], "--preset"),
        pin(
            &[
                "backtest",
                "run",
                "--local",
                "--profile",
                "run.toml",
                "--script",
                "user_data/strategies/rhai/sma_cross/sma_cross.rhai",
            ],
            "docs: --script beside --local",
        ),
        pin(
            &["backtest", "run", "--profile", "run.toml", "--write-profile", "out.toml"],
            "--write-profile",
        ),
        pin_code(
            &["backtest", "run", "--venue", "binance", "--cash", "1", "--show-effective"],
            0,
            "--show-effective prints the resolved profile and stops, dialling nothing",
        ),
        pin(&["backtest", "run", "--profile", "run.toml", "--json"], "--json after the profile"),
        pin(&["backtest", "run", "--json", "--profile", "run.toml"], "--json before the profile"),
    ]);
}

/// The search and walk-forward flags (`--rank-by`, `--optimizer` and the three method knobs).
#[test]
fn run_searches_a_parameter_space() {
    assert_accepted(&[
        pin(
            &[
                "backtest",
                "run",
                "--profile",
                "sweep.toml",
                "--optimizer",
                "tpe",
                "--trials",
                "128",
                "--addr",
                "127.0.0.1:7880",
            ],
            "docs/ops/backtesthub.md",
        ),
        pin(
            &[
                "backtest",
                "run",
                "--profile",
                "sweep.toml",
                "--rank-by",
                "SHARPE",
                "--optimizer",
                "TPE",
            ],
            "the roster is matched without case",
        ),
        pin(
            &["backtest", "run", "--profile", "sweep.toml", "--rank-by=sharpe", "--optimizer=grid"],
            "the inline spelling of both selectors",
        ),
        pin(
            &[
                "backtest",
                "run",
                "--local",
                "--profile",
                "sweep.toml",
                "--optimizer",
                "euler",
                "--euler-depth",
                "3",
            ],
            "--euler-depth",
        ),
        pin(
            &[
                "backtest",
                "run",
                "--local",
                "--profile",
                "sweep.toml",
                "--optimizer",
                "genetic",
                "--trials",
                "64",
                "--seed",
                "7",
            ],
            "--seed",
        ),
        pin(
            &[
                "backtest",
                "run",
                "--profile",
                "run.toml",
                "--rank-by",
                "multi",
                "--optimizer",
                "tpe",
            ],
            "--rank-by multi reaches both routes",
        ),
    ]);
}

/// `--local` and the data-plane sugar flags.
#[test]
fn run_local_and_the_data_sugar() {
    assert_accepted(&[
        pin(
            &["backtest", "run", "--local", "--profile", "user_data/profiles/backtest.toml"],
            "docs/ops/backtesthub.md",
        ),
        pin(
            &[
                "backtest",
                "run",
                "--local",
                "--profile",
                "run.toml",
                "--engine",
                "./backtest",
                "--json",
            ],
            "--engine names the engine outright and needs --local",
        ),
        pin(
            &["backtest", "run", "--local", "--profile", "run.toml", "--engine=./backtest"],
            "--engine, inline",
        ),
        pin(
            &["backtest", "run", "--local", "--profile", "run.toml", "--explain-data"],
            "docs: what it WILL read, then stop",
        ),
        pin(
            &["backtest", "run", "--local", "--profile", "run.toml", "--require-coverage"],
            "docs: the coverage gate",
        ),
        pin(
            &[
                "backtest",
                "run",
                "--profile",
                "run.toml",
                "--require-coverage",
                "--max-gap",
                "1d",
                "--on-gap",
                "warn",
            ],
            "docs: --max-gap and --on-gap",
        ),
        pin(&["backtest", "run", "--profile", "run.toml", "--universe", "strict"], "--universe"),
        pin(
            &["backtest", "run", "--profile", "run.toml", "--on-gap=WARN", "--universe=Covered"],
            "the data rosters are matched without case",
        ),
    ]);
}

/// Lines `run` REFUSES on purpose; they must stay refused (exit 2).
#[test]
fn run_refuses_what_it_always_refused() {
    assert_accepted(&[
        pin_code(
            &["backtest", "run", "--profile", "run.toml", "--addr"],
            2,
            "--addr needs a value",
        ),
        pin_code(
            &["backtest", "run", "--profile", "run.toml", "--store", "dir"],
            2,
            "--store is refused by name on both arms (decision 0084)",
        ),
        pin_code(
            &["backtest", "run", "--profile", "run.toml", "--store"],
            2,
            "--store with no value is refused too",
        ),
        pin_code(
            &["backtest", "run", "--profile", "run.toml", "--engine", "./backtest"],
            2,
            "--engine without --local",
        ),
        pin_code(
            &["backtest", "run", "--local", "--profile", "run.toml", "--addr", "127.0.0.1:7880"],
            2,
            "--addr beside --local",
        ),
        pin_code(&["backtest", "run"], 2, "nothing to run"),
        pin_code(
            &["backtest", "run", "--profile", "run.toml", "--json=1"],
            2,
            "a switch with a value",
        ),
        pin_code(
            &["backtest", "run", "--profile", "run.toml", "--rank-by", "nope"],
            2,
            "a bad roster word",
        ),
        pin_code(
            &["backtest", "run", "--profile", "run.toml", "--param", "a.b=1"],
            2,
            "a nested --param",
        ),
        pin_code(&["backtest", "run", "--profile", "run.toml", "--list-params"], 2, "retired flag"),
        pin_code(&["backtest", "--list-params"], 2, "retired spelling of `backtest params`"),
        pin_code(&["backtest", "--profile", "run.toml"], 2, "a flag where the sub-verb belongs"),
        pin_code(&["backtest"], 2, "the plane needs a sub-verb"),
        pin_code(&["backtest", "bogus"], 2, "an unknown sub-verb"),
        pin_code(&["backtest", "run", "--profile", "run.toml", "--bogus"], 2, "an unknown flag"),
    ]);
}

/// The stored-run reading verbs.
#[test]
fn the_reading_verbs() {
    assert_accepted(&[
        pin(&["backtest", "ls"], "docs: what is on disk"),
        pin(&["backtest", "ls", "@last"], "a selector"),
        pin(
            &["backtest", "ls", "--where", "sharpe>1", "--sort", "sharpe:asc", "--limit", "5"],
            "ls filters and ordering",
        ),
        pin(&["backtest", "ls", "--cols", "run_id,sharpe", "--json"], "ls --cols --json"),
        pin(&["backtest", "ls", "--out", "runs.txt"], "ls --out"),
        pin(
            &["backtest", "ls", "--where=sharpe>1", "--sort=sharpe", "--limit=5"],
            "inline spellings",
        ),
        pin(&["backtest", "show", "@last"], "show a run"),
        pin(&["backtest", "show", "@last", "--metrics"], "docs: show --metrics"),
        pin(&["backtest", "show", "abc123", "--trades", "--config"], "show --trades --config"),
        pin(&["backtest", "show", "@last", "--export", "equity"], "docs: the curve"),
        pin(&["backtest", "show", "@last", "--export=trades", "--out", "t.json"], "show --export"),
        pin(
            &["backtest", "show", "@last", "--html", "--out", "sheet.html"],
            "docs: the whole tearsheet",
        ),
        pin(&["backtest", "show", "--metrics-list"], "docs: the metric catalog"),
        pin(&["backtest", "show", "--metrics-list", "--out", "m.txt"], "the catalog to a file"),
        pin(&["backtest", "show", "@last", "--json"], "show --json"),
        pin(&["backtest", "path", "@last"], "path of a run"),
        pin(&["backtest", "path", "@last", "report.json"], "path of a file inside a run"),
        pin(
            &[
                "backtest",
                "tag",
                "@last",
                "--add",
                "ci",
                "--add",
                "fee-fix",
                "--note",
                "a note",
                "--as",
                "baseline/a",
                "--json",
            ],
            "tag: repeatable --add, --note, --as",
        ),
        pin(&["backtest", "tag", "@last", "--as=baseline/a"], "tag --as, inline"),
        pin(
            &[
                "backtest",
                "diff",
                "a1",
                "b2",
                "--all",
                "--changed-only",
                "--trades",
                "--md",
                "--json",
            ],
            "every diff flag",
        ),
        pin(&["backtest", "diff", "@last", "@baseline/m"], "diff two selectors"),
        pin(
            &[
                "backtest",
                "gate",
                "@last",
                "--against",
                "@baseline/m",
                "--fail-if",
                "sharpe:-5%,max_dd:+10%",
                "--json",
            ],
            "the design's own gate example",
        ),
        pin(
            &["backtest", "gate", "@last", "--against=@baseline/m", "--fail-if=sharpe:-5%"],
            "gate, inline spellings",
        ),
    ]);
}

/// The offline authoring verbs and `strategies`.
#[test]
fn the_authoring_verbs() {
    assert_accepted(&[
        pin(&["backtest", "params", "--script", "s.rhai"], "docs: params of a script"),
        pin(&["backtest", "params", "--strategy", "buy_hold"], "params of a built-in strategy"),
        pin(&["backtest", "params", "--strategy", "buy_hold", "--json"], "params --json"),
        pin(&["backtest", "params", "--script=s.rhai", "--json"], "params, inline"),
        pin(&["backtest", "strategies"], "the backtesthub's roster"),
        pin(&["backtest", "strategies", "--addr", "127.0.0.1:7880", "--json"], "strategies --addr"),
        pin(&["backtest", "strategies", "--addr=127.0.0.1:7880"], "strategies --addr, inline"),
        pin(&["backtest", "templates"], "the starter roster"),
        pin(&["backtest", "templates", "--json"], "templates --json"),
        pin(&["backtest", "templates", "sma-cross"], "print one starter"),
        pin(
            &["backtest", "templates", "sma-cross", "--write-strategy", "s.rhai"],
            "save a starter to a new file",
        ),
        pin(&["backtest", "script-api"], "everything a script may call"),
        pin(&["backtest", "script-api", "--json"], "script-api --json"),
        pin(&["backtest", "script-check", "--script", "s.rhai"], "compile one script"),
        pin(&["backtest", "script-check", "--script", "s.rhai", "--json"], "script-check --json"),
    ]);
}

/// `--help` / `-h` at every level of the plane, and the word `help`.
#[test]
fn help_at_every_level() {
    let verbs = [
        "run",
        "ls",
        "show",
        "path",
        "tag",
        "diff",
        "gate",
        "params",
        "strategies",
        "templates",
        "script-api",
        "script-check",
    ];
    let mut pins = vec![
        pin_code(&["backtest", "--help"], 0, "the plane"),
        pin_code(&["backtest", "-h"], 0, "the plane, short"),
        pin_code(&["backtest", "help"], 0, "the plane, the word"),
    ];
    for verb in verbs {
        // Leaked once per case: `Pin` holds `&'static` slices, and a test process is short-lived.
        let long: &'static [&'static str] = Box::leak(Box::new(["backtest", verb, "--help"]));
        let short: &'static [&'static str] = Box::leak(Box::new(["backtest", verb, "-h"]));
        pins.push(pin_code(long, 0, "a sub-verb's help"));
        pins.push(pin_code(short, 0, "a sub-verb's help, short"));
    }
    assert_accepted(&pins);
}
