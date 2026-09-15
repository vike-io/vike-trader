//! `--help` is a SUCCESS whose text is STDOUT — the shipped-binary gate for `vike-cli`.
//!
//! Every one of these ran the REAL binary (`CARGO_BIN_EXE_vike-cli`) rather than the parser,
//! because the defect this pins is not in the parsing: `--help` short-circuits the parser with an
//! `Err`, and each command's `run` then decided for itself what that `Err` meant. Four of them
//! decided "usage error": exit **1**, with the internal short-circuit token `"help requested"`
//! printed to **stderr** as though it were a diagnostic. That breaks `set -e`, every packaging
//! smoke test and every wrapper that checks a status — and it is invisible to a unit test of
//! `parse_args`, which sees the same `Err` either way. The three properties asserted here are
//! therefore exactly the three a user sees: the STATUS, the STREAM, and that no internal token
//! leaks.
//!
//! `vike-cli mcp` was deliberately absent from the first round — same defect, same one-line fix,
//! but `crates/vike-cli/src/cmd/mcp.rs` was owned by a concurrent branch at the time. That branch
//! merged, so its row is here now and asserts the real behaviour instead of the deferral.
//!
//! Hermetic: `VIKE_SETTINGS_DIR` points every child at an empty directory, so the dispatcher's
//! `resolve_policy` (which runs before EVERY subcommand) reads no real `policy.toml`, and the two
//! removed risk variables are cleared in case the harness inherited them.

use std::process::{Command, Output};

/// The internal short-circuit token `crate::cmd::args::help_requested` carries. It is control flow,
/// never a diagnostic, so it must never reach a user's terminal.
const SENTINEL: &str = "help requested";

/// Run the shipped binary with `args`, in an environment that resolves no project settings.
fn run(args: &[&str]) -> Output {
    // A path that need not exist: `vike_config::load` treats an absent file as "nothing
    // configured", which is exactly the neutral state these assertions want.
    let empty_settings = std::env::temp_dir().join(format!(
        "vike_cli_help_no_settings_{}_{}",
        std::process::id(),
        args.join("_").replace(['-', ' '], "")
    ));
    Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .args(args)
        .env("VIKE_SETTINGS_DIR", &empty_settings)
        .env_remove("VIKE_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
        .output()
        .unwrap_or_else(|e| panic!("run vike-cli {args:?}: {e}"))
}

/// The three properties a `--help` invocation must have, asserted together because a fix that
/// delivers only one of them is not a fix: exit 0, help on stdout, no internal token anywhere.
fn assert_help_is_clean(args: &[&str]) {
    let out = run(args);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "`vike-cli {}` must exit 0 — a non-zero --help breaks `set -e`, packaging smoke tests and \
         any wrapper that checks a status. status: {:?}, stderr: {stderr}",
        args.join(" "),
        out.status.code()
    );
    assert!(
        stdout.contains("usage:"),
        "`vike-cli {}` must print its usage to STDOUT (help is normal output, not a diagnostic); \
         stdout: {stdout:?}, stderr: {stderr:?}",
        args.join(" ")
    );
    assert!(
        !stdout.contains(SENTINEL) && !stderr.contains(SENTINEL),
        "`vike-cli {}` leaked the internal {SENTINEL:?} short-circuit token to a user; \
         stdout: {stdout:?}, stderr: {stderr:?}",
        args.join(" ")
    );
}

/// The four non-interactive remote commands plus the two settings commands, in both spellings.
/// One test per surface so a failure names the offending command instead of the first one.
#[test]
fn backtest_help_exits_zero_on_stdout() {
    assert_help_is_clean(&["backtest", "--help"]);
    assert_help_is_clean(&["backtest", "-h"]);
}

/// ⚠ **`sweep` is RETIRED and no longer has a help of its own** (ruling 13). What replaced the
/// clean-help assertion is the opposite one: the verb must FAIL, on the usage rung, with the
/// message naming what to type instead — never the generic "unknown command", which would tell a
/// scripted caller a verb that shipped for months had never existed.
#[test]
fn the_retired_sweep_verb_fails_naming_its_replacement() {
    for args in [vec!["sweep"], vec!["sweep", "--help"], vec!["sweep", "--profile", "x.toml"]] {
        let out = run(&args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stderr.contains("retired"), "{args:?} must say so: {stderr}");
        assert!(
            stderr.contains("vike-cli backtest"),
            "{args:?} must name the replacement: {stderr}"
        );
        assert!(
            !stderr.contains("unknown command"),
            "{args:?} must not read as a typo for a verb that shipped: {stderr}"
        );
    }
}

/// ⚠ **`walkforward` is RETIRED and no longer has a help of its own** (decision 3). What replaced
/// the clean-help assertion is the opposite one: the verb must FAIL, on the usage rung, with the
/// message naming what to type instead — never the generic "unknown command", which would tell a
/// scripted caller a verb that shipped for months had never existed.
#[test]
fn the_retired_walkforward_verb_fails_naming_its_replacement() {
    for args in [
        vec!["walkforward"],
        vec!["walkforward", "--help"],
        vec!["walkforward", "--profile", "x.toml"],
    ] {
        let out = run(&args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stderr.contains("retired"), "{args:?} must say so: {stderr}");
        assert!(
            stderr.contains("vike-cli backtest run"),
            "{args:?} must name the replacement: {stderr}"
        );
        assert!(
            !stderr.contains("unknown command"),
            "{args:?} must not read as a typo for a verb that shipped: {stderr}"
        );
    }
}

/// …and it is GONE from the top-level roster, because help must not advertise a verb that refuses.
/// Asserted over the rendered ROWS rather than over the raw text, for the reason
/// [`top_level_command_names`] states: a `contains` on this output is answered by the SUMMARIES.
#[test]
fn the_top_level_help_does_not_advertise_a_retired_verb() {
    let out = run(&["--help"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let names = top_level_command_names(&stdout);
    for retired in ["sweep", "walkforward", "study"] {
        assert!(
            !names.iter().any(|n| n == retired),
            "`vike-cli --help` still lists the retired `{retired}` (rows found: {names:?})"
        );
    }
}

/// Each `trade` one-shot verb rides the same shared short-circuit as its siblings, and its
/// `--help` must work with NO node and NO key — help is how an operator discovers what the command
/// needs. All three are asserted because each owns its own parser: `status` drains the shared
/// glue, while `halt`/`resume` carry a third grammar (`--reason`), and a help path that exits
/// non-zero on a KILL SWITCH is the worst place in this binary for one.
#[test]
fn the_trade_one_shot_help_exits_zero_on_stdout() {
    for verb in ["status", "halt", "resume"] {
        assert_help_is_clean(&["trade", verb, "--help"]);
        assert_help_is_clean(&["trade", verb, "-h"]);
    }
}

/// `report` and `research study` are ruling 16's two client halves, and their `--help` carries
/// more weight than most: neither can complete against any backend this workspace builds yet (both
/// server arms are follow-ups), so the usage text is the whole of what a first-time caller can
/// learn from the command. It must work with no node, no key and no backend.
#[test]
fn report_help_exits_zero_on_stdout() {
    assert_help_is_clean(&["report", "--help"]);
    assert_help_is_clean(&["report", "-h"]);
}

/// ⚠ **`study` is RETIRED off the TOP LEVEL and lives inside the RESEARCH plane** (ruling 1). Two
/// assertions, because retiring it to NOTHING would have been the wrong product: the old spelling
/// must refuse and name the new one, and the new one must have a working help of its own — no
/// daemon serves a study's wire verb, so until stage 7 lands one, being findable is the whole of
/// what this verb offers.
#[test]
fn the_retired_study_verb_fails_naming_its_replacement() {
    for args in [vec!["study"], vec!["study", "--help"], vec!["study", "--study", "cohort"]] {
        let out = run(&args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stderr.contains("retired"), "{args:?} must say so: {stderr}");
        assert!(
            stderr.contains("vike-cli research study"),
            "{args:?} must name the replacement: {stderr}"
        );
        assert!(
            stderr.contains("vike-backend study"),
            "{args:?} must keep the invocation that actually RUNS a study: {stderr}"
        );
        assert!(!stderr.contains("unknown command"), "{args:?}: {stderr}");
    }
}

#[test]
fn the_study_subcommand_help_exits_zero_on_stdout() {
    assert_help_is_clean(&["research", "study", "--help"]);
    assert_help_is_clean(&["research", "study", "-h"]);
}

/// …and the plane's own help names every sub-verb it has, so the capability stays discoverable
/// one level down. One row today; the assertion is over `crate::cmd::research`'s `SUBCOMMANDS`
/// spellings rather than a hand-written list for the reason `crate::cmd::data`'s roster test gives
/// — a hand-written copy named its roster SHORT, on the verb that deletes.
#[test]
fn the_research_help_lists_every_subcommand() {
    // ⚠ A SLICE const rather than an inline array literal: this plane's roster is ONE row today,
    // and `for sub in ["study"]` is a `clippy::single_element_loop` — `-D warnings` on the merge
    // gate. Collapsing it to a bare assertion would delete the shape the second row needs.
    const RESEARCH_SUBCOMMANDS: &[&str] = &["study"];
    let out = run(&["research", "--help"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    for sub in RESEARCH_SUBCOMMANDS {
        assert!(stdout.contains(sub), "`vike-cli research --help` must list `{sub}`: {stdout}");
    }
}

/// ⚠ A bare `vike-cli research` is a usage error naming its roster — the same rule
/// `crate::cmd::backtest` follows (there is no bare form), asserted here because this plane's
/// roster is ONE row and a one-row roster is exactly where a hand-written refusal reads fine and
/// then rots.
#[test]
fn a_bare_research_prints_usage_and_names_its_roster() {
    let out = run(&["research"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("subcommand"), "says what was expected: {stderr}");
    assert!(stderr.contains("study"), "…and names the roster: {stderr}");
}

/// The NAMES in the `commands:` section of `vike-cli --help`, one per rendered row.
///
/// ⚠ A `stdout.contains("<verb>")` assertion is NOT this question, and the difference is the whole
/// reason this helper exists: `--help` also prints every summary, and `backtest`'s reads *"run a
/// backtest on a remote vike-datahub server and print the report"* — so `contains("report")` was
/// true before the `report` verb existed and would stay true if it were deleted. `crate::COMMANDS`
/// is private to the library, so the row is read back the way a user sees it: the section between
/// `commands:` and the blank line that ends it, each row's FIRST whitespace-separated token (the
/// name is printed left-padded to a fixed width, then the summary).
fn top_level_command_names(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .skip_while(|l| l.trim() != "commands:")
        .skip(1)
        .take_while(|l| !l.trim().is_empty())
        .filter_map(|l| l.split_whitespace().next().map(str::to_string))
        .collect()
}

/// …and the TOP-LEVEL list must name it, for the reason
/// [`the_top_level_help_lists_every_command`] states one test over: a verb that exists and is not
/// in the list is one an operator can only find by reading source. It is asserted here rather than
/// appended to that test's array because `report`'s only working surface today IS discovery —
/// until its server arm lands, being findable is the whole of what it offers.
///
/// ⚠ This named TWO verbs until 2026-09-13. `study` was the second, and it is no longer a
/// top-level verb: ruling 1 makes `research` a PLANE — investigate a signal, fit a model — and a
/// study one unit of work inside it, so the spelling is `vike-cli research study`. Its
/// discoverability did not move DOWN a level and stop there: `research` is itself a `COMMANDS`
/// row, asserted by [`the_top_level_help_lists_the_research_plane`], and
/// [`the_research_help_lists_every_subcommand`] carries the second leg.
#[test]
fn the_top_level_help_lists_the_ruling_16_client_half() {
    let out = run(&["--help"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let names = top_level_command_names(&stdout);
    assert!(
        names.iter().any(|n| n == "report"),
        "`vike-cli --help` must list `report` as its own COMMANDS row (rows found: {names:?}): \
         {stdout}"
    );
}

/// ⚠ The plane itself is a top-level row. Asserted over the rendered ROWS rather than over the raw
/// text for the reason [`top_level_command_names`] states — a `contains` on this output is
/// answered by the SUMMARIES, and the word "research" appears in more than one of them.
#[test]
fn the_top_level_help_lists_the_research_plane() {
    let out = run(&["--help"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let names = top_level_command_names(&stdout);
    assert!(
        names.iter().any(|n| n == "research"),
        "`vike-cli --help` must list `research` as its own COMMANDS row (rows found: {names:?}): \
         {stdout}"
    );
}

/// `config` has THREE help paths now and two of them once disagreed: the bare verb
/// (`config --help`) was already correct, while `config show --help` went through the shared parser
/// short-circuit and hit the same exit-1-with-sentinel arm as `backtest`. All three are asserted so
/// the set cannot drift again — and `check`'s matters most of the three, because it is the verb a
/// shipped `deploy/*.service` unit runs, where a `--help` probe that exits non-zero reads as a
/// broken binary.
#[test]
fn config_help_exits_zero_on_stdout_at_every_level() {
    assert_help_is_clean(&["config", "--help"]);
    assert_help_is_clean(&["config", "show", "--help"]);
    assert_help_is_clean(&["config", "show", "-h"]);
    assert_help_is_clean(&["config", "check", "--help"]);
    assert_help_is_clean(&["config", "check", "-h"]);
    assert_help_is_clean(&["config", "set", "--help"]);
    assert_help_is_clean(&["config", "set", "-h"]);
}

/// The verb list is the only discovery surface `config` has, so `config --help` must NAME every
/// verb. A verb that exists and is not in the list is one an operator can only find by reading
/// source — which is how `check` would have shipped invisible.
#[test]
fn the_config_help_lists_every_verb() {
    let out = run(&["config", "--help"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    for verb in ["show", "check", "set"] {
        assert!(stdout.contains(verb), "`config --help` must list `{verb}`: {stdout}");
    }
}

/// …and the top-level list must too, for the same reason one level up.
///
/// ⚠ Asserted over [`top_level_command_names`] rather than over the raw text, and that changed
/// here: a `contains` on this output is answered by the SUMMARIES as well as the rows, so several
/// of these names were being confirmed by a sentence rather than by a verb (`backtest`'s summary
/// alone contains "backtest", "report" and "server"). Reading the row back is the assertion the
/// doc above always claimed.
#[test]
fn the_top_level_help_lists_every_command() {
    let out = run(&["--help"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let names = top_level_command_names(&stdout);
    // ⚠ `strategy-status` is NOT in this list and was, in the branch that introduced the row-based
    // assertion. Ruling 17 of the datahub-market-data-wire design folded that top-level verb into
    // the `trade` family (`trade status`), so `COMMANDS` no longer carries a row for it and this
    // loop asserting one would fail for the right reason on a name that is gone.
    for command in ["backtest", "config", "data", "secrets", "trade", "init", "indicators"] {
        assert!(
            names.iter().any(|n| n == command),
            "`vike-cli --help` must list `{command}` as its own COMMANDS row (rows found: \
             {names:?}): {stdout}"
        );
    }
}

/// `secrets` already exited 0 — the defect here is the STREAM: it printed its usage with
/// `eprintln!`, so `vike-cli secrets --help | less` showed an empty page.
#[test]
fn secrets_help_exits_zero_on_stdout_at_both_levels() {
    assert_help_is_clean(&["secrets", "--help"]);
    assert_help_is_clean(&["secrets", "list", "-h"]);
}

/// `mcp` is the stdio MCP server, so it is the one surface where a broken `--help` is worst: an
/// agent host that probes a server with `--help` before wiring it up reads exit 1 + a bare
/// `help requested` on stderr as "this server is broken". It inlined the error arm rather than
/// sharing `args::exit_for_parse_error`, which is exactly how the spellings drifted the first time.
#[test]
fn mcp_help_exits_zero_on_stdout() {
    assert_help_is_clean(&["mcp", "--help"]);
    assert_help_is_clean(&["mcp", "-h"]);
}

/// `data` is the verb a new user reaches for straight after `init`, and its help is the ONLY place
/// its subcommands are named — the verb itself takes no default action. A `--help` that failed
/// there would leave "get some market data" with no discoverable answer at all. (It said "the two
/// subcommands"; ruling 12 moved four more here, and `crates/vike-cli/tests/data_cli.rs`'s
/// `help_names_every_subcommand_and_exits_zero` is the one that holds the roster — so this doc
/// names no count, which is the claim that had already rotted.)
#[test]
fn data_help_exits_zero_on_stdout_at_both_levels() {
    assert_help_is_clean(&["data", "--help"]);
    assert_help_is_clean(&["data", "fetch", "-h"]);
}

/// `indicators` prints the callable indicator roster, so a broken `--help` on it is the same class
/// of defect as on `mcp`: the command exists to be DISCOVERED — the shipped
/// `user_data/strategies/rhai/README.md` points a user straight at it — and a `--help` that exits 1
/// with a bare token reads as "this command is broken" to the person following that pointer.
#[test]
fn indicators_help_exits_zero_on_stdout() {
    assert_help_is_clean(&["indicators", "--help"]);
    assert_help_is_clean(&["indicators", "-h"]);
}

/// The top-level dispatcher and the interactive `trade` command were the two that were already
/// right. They are pinned so the fix cannot regress the reference behaviour it was modelled on.
#[test]
fn the_already_correct_surfaces_stay_correct() {
    assert_help_is_clean(&["--help"]);
    assert_help_is_clean(&["-h"]);
    assert_help_is_clean(&["help"]);
    assert_help_is_clean(&["trade", "--help"]);
}

/// `--version`/`-V`: the version on stdout, exit 0. It was unrecognised, so it fell into the
/// unknown-command arm — an exit 1 with `unknown command '--version'` on stderr.
#[test]
fn version_prints_the_crate_version_on_stdout() {
    for flag in ["--version", "-V"] {
        let out = run(&[flag]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "`vike-cli {flag}` must exit 0; status: {:?}, stderr: {stderr}",
            out.status.code()
        );
        assert!(
            stdout.contains(env!("CARGO_PKG_VERSION")),
            "`vike-cli {flag}` must print the crate version {:?} on stdout; stdout: {stdout:?}",
            env!("CARGO_PKG_VERSION")
        );
        assert!(
            !stderr.contains("unknown command"),
            "`vike-cli {flag}` must be recognised, not routed to the unknown-command arm; \
             stderr: {stderr:?}"
        );
    }
}

/// The negative half, so "exit 0 on --help" is never bought by making everything exit 0: a real
/// usage error still fails, and still explains itself on stderr.
#[test]
fn a_real_usage_error_still_fails_on_stderr() {
    let out = run(&["backtest", "--not-a-flag"]);
    assert!(!out.status.success(), "an unknown flag must still exit non-zero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("usage:"), "a usage error keeps printing usage to stderr: {stderr:?}");
    assert!(!stderr.contains(SENTINEL), "and it must not carry the help token either: {stderr:?}");

    let out = run(&["no-such-command"]);
    assert!(!out.status.success(), "an unknown command must still exit non-zero");
}
