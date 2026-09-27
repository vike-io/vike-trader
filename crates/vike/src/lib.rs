//! `vike-backend` — the multi-call dispatcher's pure core: the tool table, the context every tool is
//! handed, and the argv routing.
//!
//! ⚠ The crate lives at `crates/vike/` and is PACKAGED as `vike-backend`, so the lib is
//! `vike_backend` and the shipped executable is `vike-backend`. The directory kept its old name
//! deliberately: `crates/vike-ops/tests/multicall_gate.rs` keys on the PATH (`crates/vike/src`,
//! `crates/vike/Cargo.toml`) and so does the root manifest's `[workspace].members` entry, so moving
//! it would put a directory rename in the same commit as a package rename, an artifact rename and a
//! verb rename — four independent failure modes sharing one bisect.
//!
//! # Why this crate exists
//!
//! MEASURED on the CI box, 2026-09-01, release profile: `vike-tradehub` 128 MB, `backtest` 106 MB,
//! `vike-datahub` 107 MB — while `vike-cli` is 9.6 MB and `tearsheet` 1.9 MB. The gap is one static
//! copy of DataFusion/arrow/parquet **per binary**, and the container image published by
//! `docs/decisions/0035-the-image-ships-every-feature-and-may-be-the-primary-install.md` carries
//! all of them. Folding the tools into ONE executable links that closure once.
//!
//! # The shape, and the one rule that makes it safe
//!
//! ⚠ **ONE TOOL PER PROCESS.** Every tool in this workspace owns process-global state that assumes
//! it is alone: `vike_log::init` (a `try_init` where only the first `LogConfig` wins),
//! `vike_ops::stop::install_handlers`, `vike_script`'s `INSTALLED` one-shot, and a dozen
//! `OnceLock`s besides. A `vike-backend a && vike-backend b` that ran both in one process would
//! silently give the second tool the first one's logging configuration and signal handlers. There
//! is no safe version of that, so [`Router::dispatch`] refuses a second call rather than
//! documenting the hazard, and `crates/vike-ops/tests/multicall_gate.rs` holds the refusal.
//!
//! # ⚠ The dispatcher itself does NOTHING
//!
//! No boot, no logging, no signal handler, no project resolution. That restraint is load-bearing
//! rather than tidy: every tool calls `vike_boot::boot` for itself, and a dispatcher that also
//! resolved the project would make one process perform the settings walk TWICE — where the second
//! walk is `$VIKE_SETTINGS_DIR`-blind and answers from whatever directory the process happens to
//! sit above. That is the defect that put a the CI box daemon on no policy, no config and no
//! credentials, every venue silently on paper.
//!
//! ⚠ **`crates/vike-boot/tests/one_owner.rs` cannot see that failure.** Its
//! `a_crate_that_boots_may_not_walk_again` derives its roster from the crate DIRECTORIES containing
//! a `vike_boot::boot(` call, so a dispatcher that boots while a tool also boots is invisible to
//! it — both are "one crate booting once". `multicall_gate` is the only thing standing in front of
//! it, which is why that gate reads this crate's source rather than trusting this comment.
//!
//! # The context is a PARAMETER, and that is the point
//!
//! [`ToolCtx`] carries the ONE `std::env::vars()` sweep and the ONE `current_dir()` for the whole
//! process. `crates/vike-ops/tests/settings_registry.rs` asks exactly this of every library —
//! *"libraries take configuration as parameters; only binaries read the process environment"* — so
//! threading the sweep through here moves reads from `Layer::Binary` to `Layer::Injected`, the
//! registry's declared target state, rather than fighting the ratchet that guards it.

use std::collections::HashMap;
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

/// What every tool is handed instead of reading the process itself.
///
/// Borrowed rather than owned so the dispatcher's single sweep is not cloned per tool, and so a
/// tool cannot mutate what a later reader would see.
#[derive(Debug, Clone, Copy)]
pub struct ToolCtx<'a> {
    /// The whole environment, swept ONCE by the dispatcher's `main`.
    pub env: &'a HashMap<String, String>,
    /// The working directory, resolved ONCE. `None` when it could not be read — the same shape
    /// `std::env::current_dir().ok()` produces, kept rather than defaulted so a tool can tell
    /// "no directory" from "the root".
    pub cwd: Option<&'a Path>,
    /// The name this tool was invoked as, for usage strings. Under argv[0] dispatch this is the
    /// link's basename; under verb dispatch it is the verb. ⚠ Never `current_exe()`: under a
    /// symlink Linux resolves that to the dispatcher, so every tool would print `vike-backend`.
    pub invoked_as: &'a str,
}

/// One tool's entry point. Takes the context and the arguments AFTER its own name.
pub type ToolMain = fn(&ToolCtx<'_>, &[String]) -> ExitCode;

/// A row in the tool table.
pub struct Tool {
    /// The name this tool answers to, as a verb and as an argv[0] basename.
    ///
    /// ⚠ **This string IS the deployed interface, in both shapes at once**: it is what follows
    /// `vike-backend` on a command line, and it is the BASENAME of any link installed for this
    /// tool (`scripts/release_container_image.sh` stages one per row into the image's
    /// `/opt/vike/bin`). So renaming a row renames its link, and the two must move in one commit.
    ///
    /// ⚠ This used to read *"must equal the binary name it replaces"*, and that contract is
    /// RETIRED: the verbs deliberately dropped the redundant `vike-` prefix the dispatcher already
    /// carries (`vike-backend trade`, not `vike-backend vike-tradehub`), so four of these names
    /// no longer equal the per-tool binary they descend from. What replaces it is the sentence
    /// above — the name is a fact about DEPLOYMENT, not about a legacy filename.
    ///
    /// ⚠ The failure of getting it wrong is QUIET, not a link error. [`resolve`] matches by exact
    /// string with no aliases and no fallback, so a link or a unit naming a string this table does
    /// not carry falls through to `Route::Usage(2)`: exit 2 with a tool list on stderr, from a
    /// process that started and did nothing. Anything treating "the process ran" as success reads
    /// that as fine.
    pub name: &'static str,
    pub main: ToolMain,
    /// One line for `--help`.
    pub summary: &'static str,
}

/// What [`resolve`] decided.
#[derive(Debug, PartialEq, Eq)]
pub enum Route<'a> {
    /// Run this tool's index in the table, with these arguments.
    Tool { index: usize, args: &'a [String] },
    /// Print the tool list and exit with this code — 0 when asked for, 2 when the argv made no
    /// sense.
    Usage(u8),
    /// Print the dispatcher's own version and exit 0. Every tool behind the table answers
    /// `--version` for itself (through its own parser), so the bare `vike-backend --version`
    /// answering a usage ERROR was the one inconsistency — and the image smoke runs `--version`
    /// through every link, the dispatcher's included.
    Version,
}

/// The basename of an argv[0], with a Windows `.exe` suffix removed.
///
/// ⚠ Both halves matter. Without the basename, an absolute `ExecStart=/srv/vike/bin/trade`
/// never matches a table row; without the suffix strip, the same install on Windows never matches.
#[must_use]
pub fn program_name(argv0: &str) -> &str {
    let base = argv0.rsplit(['/', '\\']).next().unwrap_or(argv0);
    base.strip_suffix(".exe").unwrap_or(base)
}

/// Decide what to run. PURE — no environment, no filesystem, no process state, so the whole
/// grammar is unit-tested below.
///
/// Two shapes and deliberately no others:
///
/// 1. **argv[0] names a tool** — the installed-symlink shape. Arguments are `argv[1..]`, so a tool
///    reached this way sees exactly what it saw as its own binary.
/// 2. **argv[0] is anything else** — the `vike-backend <verb>` shape. `argv[1]` selects, arguments
///    are `argv[2..]`.
///
/// ⚠ Shape 1 is tried FIRST, and it has to be: the dispatcher will be installed under every tool's
/// name, so `tradehub --config x` must route by argv[0] rather than trying to read `--config`
/// as a verb.
#[must_use]
pub fn resolve<'a>(tools: &[Tool], argv: &'a [String]) -> Route<'a> {
    let Some(argv0) = argv.first() else {
        // No argv at all is not reachable from a real exec, but returning a usage code keeps this
        // function total rather than panicking somewhere a test cannot reach.
        return Route::Usage(2);
    };
    let invoked = program_name(argv0);
    if let Some(index) = tools.iter().position(|t| t.name == invoked) {
        return Route::Tool { index, args: &argv[1..] };
    }
    match argv.get(1).map(String::as_str) {
        None => Route::Usage(2),
        Some("-h" | "--help" | "help") => Route::Usage(0),
        Some("-V" | "--version") => Route::Version,
        Some(verb) => match tools.iter().position(|t| t.name == verb) {
            Some(index) => Route::Tool { index, args: &argv[2..] },
            None => Route::Usage(2),
        },
    }
}

/// Guards the one-tool-per-process rule.
///
/// A free function over a `static` rather than a value the caller holds, because the hazard is
/// process-global and a caller who could construct a second `Router` would defeat it.
static DISPATCHED: AtomicBool = AtomicBool::new(false);

/// Run one tool, once.
///
/// # Panics
///
/// On a SECOND call in the same process. That is deliberate and is not a defensive assertion: the
/// tools share `OnceLock`-shaped global state, so a second dispatch would run with the first
/// tool's logging configuration and signal handlers — wrong quietly, in a process that may be
/// signing orders. A panic is the only outcome that cannot be mistaken for working.
pub fn dispatch(tool: &Tool, ctx: &ToolCtx<'_>, args: &[String]) -> ExitCode {
    assert!(
        !DISPATCHED.swap(true, Ordering::SeqCst),
        "vike-backend: a second tool was dispatched in one process. Every tool here owns \
         process-global state that assumes it is alone (vike_log::init keeps only the first \
         LogConfig, stop::install_handlers registers once, vike_script's INSTALLED is a \
         one-shot), so the second would silently inherit the first's. Run one tool per process."
    );
    (tool.main)(ctx, args)
}

/// Render the tool list for `--help`. Separate from printing so it is testable.
#[must_use]
pub fn usage(tools: &[Tool]) -> String {
    let mut out = String::from(
        "vike-backend — one executable, every headless vike tool.\n\n\
         usage: vike-backend <tool> [args...]\n\
        \x20      ...or invoke it under a tool's own name (installed as a link).\n\n\
         tools:\n",
    );
    for t in tools {
        out.push_str(&format!("  {:<16}{}\n", t.name, t.summary));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(_ctx: &ToolCtx<'_>, _args: &[String]) -> ExitCode {
        ExitCode::SUCCESS
    }

    fn table() -> Vec<Tool> {
        vec![
            Tool { name: "backtest", main: t, summary: "run a backtest" },
            Tool { name: "trade", main: t, summary: "the live daemon" },
        ]
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn a_basename_is_taken_from_a_full_path_on_both_separators() {
        assert_eq!(program_name("/srv/vike/bin/trade"), "trade");
        assert_eq!(program_name(r"C:\opt\vike\bin\backtest.exe"), "backtest");
        assert_eq!(program_name("backtest"), "backtest");
    }

    /// The installed-symlink shape: an absolute path under a tool's name routes to that tool and
    /// its arguments are untouched.
    #[test]
    fn argv0_names_the_tool_and_keeps_every_argument() {
        let tools = table();
        let a = argv(["/srv/vike/bin/trade", "--config", "x.toml"].as_slice());
        match resolve(&tools, &a) {
            Route::Tool { index, args } => {
                assert_eq!(tools[index].name, "trade");
                assert_eq!(args, &a[1..]);
            }
            other => panic!("{other:?}"),
        }
    }

    /// ⚠ argv[0] must win over the verb shape, or `trade --config x` would try to read
    /// `--config` as a tool name and print usage instead of starting the daemon.
    #[test]
    fn argv0_wins_over_the_verb_shape() {
        let tools = table();
        let a = argv(["backtest", "trade"].as_slice());
        match resolve(&tools, &a) {
            Route::Tool { index, args } => {
                assert_eq!(tools[index].name, "backtest", "argv[0] must decide");
                assert_eq!(args.len(), 1, "the verb-looking argument stays an argument");
            }
            other => panic!("{other:?}"),
        }
    }

    /// The verb shape drops BOTH the program name and the verb — a leftover verb is inert in
    /// tools that scan for exact `--flag` tokens, so the bug would be invisible in some and fatal
    /// in others. Strip once, here.
    #[test]
    fn the_verb_shape_drops_the_program_name_and_the_verb() {
        let tools = table();
        // ⚠ The program name must stay a string that matches NO row, or this stops testing the
        // verb shape and quietly becomes an argv[0] test.
        let a = argv(["vike-backend", "backtest", "--from", "2026-01-01"].as_slice());
        match resolve(&tools, &a) {
            Route::Tool { index, args } => {
                assert_eq!(tools[index].name, "backtest");
                assert_eq!(args, &a[2..]);
                assert!(!args.contains(&"backtest".to_string()), "the verb must not survive");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn help_exits_zero_and_an_unknown_verb_exits_two() {
        let tools = table();
        assert_eq!(resolve(&tools, &argv(["vike-backend", "--help"].as_slice())), Route::Usage(0));
        assert_eq!(resolve(&tools, &argv(["vike-backend", "nope"].as_slice())), Route::Usage(2));
        assert_eq!(resolve(&tools, &argv(["vike-backend"].as_slice())), Route::Usage(2));
        assert_eq!(resolve(&tools, &[]), Route::Usage(2));
    }

    /// The dispatcher answers `--version` like every tool behind it does — the image smoke runs
    /// `--version` through EVERY link including the bare `vike-backend`, so a usage-error answer
    /// here would fail a correctly built image. Under a TOOL's name the flag stays the tool's to
    /// answer: `tradehub --version` must reach tradehub's parser, not this arm.
    #[test]
    fn the_dispatcher_answers_version_and_a_tool_link_keeps_the_flag() {
        let tools = table();
        assert_eq!(
            resolve(&tools, &argv(["vike-backend", "--version"].as_slice())),
            Route::Version
        );
        assert_eq!(resolve(&tools, &argv(["vike-backend", "-V"].as_slice())), Route::Version);
        match resolve(&tools, &argv(["/opt/vike/bin/trade", "--version"].as_slice())) {
            Route::Tool { args, .. } => assert_eq!(args, ["--version".to_string()].as_slice()),
            other => panic!("a tool link must keep --version for the tool: {other:?}"),
        }
    }

    #[test]
    fn usage_names_every_tool() {
        let tools = table();
        let text = usage(&tools);
        for t in &tools {
            assert!(text.contains(t.name), "usage omits {}", t.name);
        }
    }
}

#[cfg(test)]
mod arg_passing_tests {
    use super::*;

    /// ⚠ The tail a tool receives is EXACTLY what it would have seen as its own binary — no more,
    /// no less.
    ///
    /// This exists because the first `vike-cli` row got it wrong in a way nothing else would have
    /// caught. It wrote `args.iter().skip(1)`, copying the standalone shim where argv[0] is still
    /// attached — but [`resolve`] has already stripped it. The extra skip eats the first REAL
    /// argument, so `vike-cli config check` becomes `vike-cli check`: a different verb, which
    /// parses, runs and reports success. No type catches it and no gate reads it.
    #[test]
    fn a_tool_receives_its_own_argv_tail_and_nothing_is_eaten() {
        fn probe(_c: &ToolCtx<'_>, _a: &[String]) -> std::process::ExitCode {
            std::process::ExitCode::SUCCESS
        }
        let tools = [Tool { name: "vike-cli", main: probe, summary: "" }];
        let want: Vec<String> = ["config", "check"].iter().map(|s| (*s).to_string()).collect();

        // argv[0] shape — the installed link. The tail is everything after the program name.
        let via_link: Vec<String> = ["/opt/vike/bin/vike-cli", "config", "check"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        match resolve(&tools, &via_link) {
            Route::Tool { args, .. } => {
                assert_eq!(args, &want[..], "argv[0] shape ate an argument")
            }
            other => panic!("{other:?}"),
        }

        // verb shape — the tail is everything after the program name AND the verb, and it must be
        // the SAME tail, or one invocation of a tool differs from the other.
        // ⚠ `vike-cli` keeps its prefix as a verb AND as a link name — it is a separately shipped
        // binary named on six `ExecStartPre=` lines, so it was deliberately left out of the
        // prefix drop. Only the PROGRAM name moved.
        let via_verb: Vec<String> = ["vike-backend", "vike-cli", "config", "check"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        match resolve(&tools, &via_verb) {
            Route::Tool { args, .. } => assert_eq!(args, &want[..], "verb shape ate an argument"),
            other => panic!("{other:?}"),
        }
    }
}
