//! The multi-call dispatcher binary. See `vike-backend`'s lib doc for why it exists and what it may
//! not do.
//!
//! ⚠ This file performs the process's ONE `std::env::vars()` sweep and its ONE `current_dir()`, and
//! then does NOTHING else before handing control to a tool. It must not boot, must not initialise
//! logging, must not install signal handlers and must not resolve the project: every tool does its
//! own `vike_boot::boot`, and a second walk here would be `$VIKE_SETTINGS_DIR`-blind.
//! `crates/vike-ops/tests/multicall_gate.rs` reads this file to hold that.

use std::collections::HashMap;
use std::process::ExitCode;

use vike_backend::{Route, Tool, ToolCtx, dispatch, resolve, usage};

/// The tools this executable carries.
///
/// ⚠ **Each row is feature-gated exactly as the binary it replaces.** A tool compiled in
/// unconditionally would drag its dependency closure into every build of every other tool, and
/// `vike-studio-core`'s `study-cli` is the clearest case: it is OFF by default so that crate's
/// normal-dep tree carries no datafusion, a property `scripts/ci_feature_suite.sh`'s
/// `studio-standalone` lane checks STRUCTURALLY rather than by compiling.
///
/// ⚠ **`name` and the `#[cfg]` feature are two different strings on most of these rows, and that is
/// deliberate.** (No count here: this line carried one, and ruling 10 retiring the `recorder` row
/// was the second time in a month it would have gone stale.) The FEATURE names the optional
/// dependency it switches on, whose package really is
/// `vike-tradehub`/`vike-datahub`; the NAME is what an operator types after
/// `vike-backend` and what an installed link is called, where the `vike-` prefix was redundant with
/// the binary already carrying it. Do not "align" them — `crates/vike/Cargo.toml`'s `full` and
/// `.github/workflows/release.yml`'s `--features` line both spell the FEATURE, and
/// `scripts/release_container_image.sh`'s link roster spells the NAME.
///
/// ⚠ **`vike-cli` keeps its prefix, as a name AND as a feature.** It is a separately shipped
/// binary as well as a row here, and stripping it to `cli` would rename the link the pre-flight
/// below runs.
const TOOLS: &[Tool] = &[
    #[cfg(feature = "study-cli")]
    Tool {
        name: "study",
        main: study_cli_main,
        summary: "run a compiled study over the hist store",
    },
    // ⚠ The VERB is `report`; the FEATURE stays `tearsheet`. Two different names on purpose, and
    // this is the one row where they diverge.
    //
    // `tearsheet` is a real term of art — a one-page strategy performance summary, the word
    // `pyfolio` and `quantstats` both use — so it is right in the crate name (`vike-report` renders
    // one), in `--help` and in the output. What it is NOT is a good thing to make somebody TYPE: it
    // names ONE shape of report, so a second report kind would make the verb wrong, and a reader who
    // has not met the term cannot guess it. The crate was already called `vike-report` while the
    // verb said `tearsheet`, so the tree disagreed with itself before this change; renaming the verb
    // resolves that rather than creating it.
    #[cfg(feature = "tearsheet")]
    Tool {
        name: "report",
        main: tearsheet_main,
        summary: "render a tearsheet from a live journal",
    },
    #[cfg(feature = "backtest")]
    Tool { name: "backtest", main: backtest_main, summary: "run a backtest and mint a run" },
    // ⚠ **`recorder` IS GONE FROM THIS TABLE and its work is here** — ruling 10
    // (`docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` §0.5) merged the
    // recorder daemon into the data server, so one process owns venue connections, the store and
    // serving. The tape is now `vike-backend datahub --record <profile>`; `vike-backend recorder`
    // is `Route::Usage(2)` — `crates/vike/src/lib.rs`'s `resolve` matches by EXACT string with no
    // aliases, so a unit still naming it prints this dispatcher's tool list and exits 2 rather than
    // failing to exec. `docs/ops/recorder-deploy.md` carries the operator's flip, in order, so the
    // recorder never stops recording.
    #[cfg(feature = "vike-datahub")]
    Tool {
        name: "datahub",
        main: datahub_main,
        summary: "serve the hist store, and with --record fill it from the venues",
    },
    // ⚠ `trade`, not `tradehub`, and this is the one verb whose rename can take the live
    // order-signing daemon down — it is the `ExecStart=` of two tracked units.
    //
    // The `hub` suffix went from `datahub` and the retired `recorder` in the same cut; leaving it
    // on this one
    // would have kept the odd row out. And it pairs the CLIENT with the DAEMON under one word:
    // `crates/vike-cli/src/cmd/trade.rs` is already "a HUMAN interactive REPL over a running
    // vike-tradehub node", reaching this very daemon through `RemoteCoreHandle`/`RemoteControlHandle`
    // — the same operation, previously spelled two ways depending on which side you stood.
    //
    // ⚠ The CRATE stays `vike-tradehub` and so does the FEATURE. Renaming a crate is a different
    // change with a different blast radius, and the verb is what an operator types.
    #[cfg(feature = "vike-tradehub")]
    Tool { name: "trade", main: tradehub_main, summary: "the live trading daemon" },
    #[cfg(feature = "vike-cli")]
    Tool {
        name: "vike-cli", main: cli_main, summary: "the operator CLI — config, secrets, trade"
    },
];

/// ⚠ **The one whose absence is a deployment failure, not a missing feature.**
/// `ExecStartPre=<root>/bin/vike-cli config check` is named by every daemon unit under `deploy/`, and
/// `deploy/docker/entrypoint.sh` runs it too. It was missing from this table until a real run
/// caught it: the staged `vike-cli` link resolved to a binary that printed the tool list and exited
/// 2, so the pre-flight would have failed every start — the exact `203/EXEC`-shaped hazard this
/// merge is supposed to avoid, wearing exit 2 instead.
///
/// ⚠ Takes argv only. `vike_cli::run` performs its own `vike_boot::boot`, and its `BootSpec` is the
/// one that REFUSES removed environment variables where the data daemon ignores them —
/// flattening that difference by feeding it a shared context would either start refusing where
/// nothing did, or stop refusing where an operator believes a ceiling is armed.
/// ⚠ **NO `.skip(1)` here, and the first draft had one.** `resolve` has ALREADY removed the program
/// name (argv[0] shape) or the program name AND the verb (`vike-backend <tool>` shape), so `args`
/// is the tail every other tool receives. A second skip silently eats the first real argument — it
/// would have turned `vike-cli config check` into `vike-cli check`, which parses, runs a different
/// verb and reports success. The standalone shim's `std::env::args().skip(1)` is where that skip
/// belongs, because there argv[0] is still attached.
#[cfg(feature = "vike-cli")]
fn cli_main(_ctx: &ToolCtx<'_>, args: &[String]) -> ExitCode {
    vike_cli::run(args.iter().cloned())
}

/// ⚠ The daemon, and the one tool where the ORDER of what happens next is a safety property.
/// `tradehub_cli::run` installs the signal handlers as its first statement, before anything can
/// mount a venue or place an order, and `crates/vike-ops/tests/graceful_stop_pin.rs` checks that
/// positionally. **This dispatcher must add nothing before that call** — an unhandled window
/// already exists before any `main`'s first statement, but anything added here would widen it.
#[cfg(feature = "vike-tradehub")]
fn tradehub_main(ctx: &ToolCtx<'_>, args: &[String]) -> ExitCode {
    vike_tradehub::tradehub_cli::run(ctx.env, ctx.cwd, args)
}

/// ⚠ The only tool so far that also needs the WORKING DIRECTORY. Its store-root resolution walks
/// from there, and the moved body read `current_dir()` twice; the dispatcher reads it once and
/// hands the same answer to both sites.
///
/// ⚠ It is also where the RECORDER went (ruling 10), which is why the environment map matters here
/// twice over: the recorder read it at TWO instants before the multicall merge — once in its own
/// `main`, once inside `webhook_targets` to overlay the credential store — and now takes the one
/// map this dispatcher swept, which is also the map its boot consumed.
#[cfg(feature = "vike-datahub")]
fn datahub_main(ctx: &ToolCtx<'_>, args: &[String]) -> ExitCode {
    vike_datahub::datahub_cli::run(ctx.env, ctx.cwd, args)
}

/// ⚠ Supplies the CLOCK as well as the environment. `crates/vike-ops/tests/clock_pin.rs`'s
/// `CLOCK_PIN` keeps `SystemTime::now()` out of the library tree, so `backtest_cli::run` takes the
/// timestamp as a parameter and a composition root reads it. This dispatcher IS that root.
///
/// ⚠ A clock set before the epoch answers with a NEGATIVE second rather than `0` — zero is a real
/// instant, and a failure wearing a valid value is how a run manifest starts lying about when it
/// happened. Kept identical to the shipped bin's own reader, deliberately.
#[cfg(feature = "backtest")]
fn backtest_main(ctx: &ToolCtx<'_>, args: &[String]) -> ExitCode {
    fn now_unix_secs() -> i64 {
        match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => d.as_secs() as i64,
            Err(before) => -(before.duration().as_secs() as i64),
        }
    }
    // ⚠ THE STUDIO MOUNT (ruling 7). This dispatcher is the ONLY composition root that can name
    // both halves: `vike_backtest::compute_server` declares the table type (layer 50) and
    // `vike_studio_core::studio_run_table` fills it (layer 55), and 50 may not see 55 — the edge
    // runs the other way, so it never could. Passing it here is what makes `vike-backend backtest
    // --addr` serve all SEVEN compute verbs where the standalone `backtest` bin serves four and
    // refuses the Studio three by name. Same shape as `vike_datahub`'s `real_backfill_table`: the
    // server declares a feature-free seam, the root fills it.
    vike_backtest::backtest_cli::run(
        ctx.env,
        args,
        &now_unix_secs,
        Some(vike_studio_core::studio_run_table()),
    )
}

/// ⚠ Takes the context and USES it: `tearsheet` reads `VIKE_JOURNAL_DIR`, and taking it from
/// `ctx.env` rather than `std::env::var` is what keeps that read out of the LIBRARY_PIN ratchet —
/// its registry row is `Layer::Injected` for exactly this reason.
#[cfg(feature = "tearsheet")]
fn tearsheet_main(ctx: &ToolCtx<'_>, args: &[String]) -> ExitCode {
    vike_report::tearsheet_cli::run(ctx.env, args)
}

/// ⚠ Takes no context, and that is a fact about the tool rather than an omission: `study`
/// reads no environment variable at all — it takes eight `--flags` and nothing else — so there is
/// nothing for `ToolCtx` to carry. The parameter stays in the signature because [`ToolMain`] is one
/// type for every tool.
#[cfg(feature = "study-cli")]
fn study_cli_main(_ctx: &ToolCtx<'_>, args: &[String]) -> ExitCode {
    vike_studio_core::study_cli::run(args)
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().collect();
    let env: HashMap<String, String> = std::env::vars().collect();
    let cwd = std::env::current_dir().ok();
    match resolve(TOOLS, &argv) {
        Route::Tool { index, args } => {
            let tool = &TOOLS[index];
            let ctx = ToolCtx { env: &env, cwd: cwd.as_deref(), invoked_as: tool.name };
            dispatch(tool, &ctx, args)
        }
        Route::Usage(code) => {
            // Code 0 means the user ASKED, so it belongs on stdout where `| less` can see it; a
            // refusal belongs on stderr. Same split `vike-cli` already makes.
            if code == 0 {
                print!("{}", usage(TOOLS));
            } else {
                eprint!("{}", usage(TOOLS));
            }
            ExitCode::from(code)
        }
        Route::Version => {
            // Stdout + exit 0 for the same reason `--help`'s is: the user asked. Through
            // `version_line`, never a bare name+version — `identity_adoption.rs` refuses a printer
            // that cannot say which commit it was built from.
            println!(
                "{}",
                vike_buildinfo::version_line(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
            );
            ExitCode::SUCCESS
        }
    }
}
