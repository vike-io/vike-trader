//! The multi-call dispatcher binary. See `vike-backend`'s lib doc for why it exists and what it may
//! not do.
//!
//! ⚠ This file performs the process's ONE `std::env::vars()` sweep and its ONE `current_dir()`, and
//! then does NOTHING else before handing control to a tool. It must not boot, must not initialise
//! logging, must not install signal handlers and must not resolve the project: every tool does its
//! own `vike_boot::boot`, and a second walk here would be `$VIKE_SETTINGS_DIR`-blind.
//! `crates/vike-ops/tests/container_deploy/multicall_gate.rs` reads this file to hold that.

use std::collections::HashMap;
use std::process::ExitCode;

use vike_backend::{Tool, ToolContext, ToolRoute, dispatch, resolve, usage};

/// The tools this executable carries.
///
/// ⚠ **Each row is feature-gated exactly as the binary it replaces.** A tool compiled in
/// unconditionally would drag its dependency closure into every build of every other tool, and
/// `vike-studio-core`'s `study-cli` was the clearest case: OFF by default so that crate's
/// normal-dep tree carried no datafusion (`scripts/ci_feature_suite.sh`'s `studio-standalone` lane
/// checks it STRUCTURALLY); the study CLI is `vike-backtesthub`'s since 2026-11-10 and the gate is that
/// row's feature now.
///
/// ⚠ **`name` and the `#[cfg]` feature are two different strings on most of these rows, and that is
/// deliberate.** (No count here: this line carried one, and ruling 10 retiring the `recorder` row
/// was the second time in a month it would have gone stale.) The FEATURE names the optional
/// dependency it switches on, whose package really is
/// `vike-tradehub`/`vike-datahub`; the NAME is what an operator types after
/// `vike-backend` and what an installed link is called, where the `vike-` prefix was redundant with
/// the binary already carrying it. Do not "align" them — `crates/vike-backend/Cargo.toml`'s `full` and
/// `.github/workflows/release.yml`'s `--features` line both spell the FEATURE, and
/// `scripts/release_container_image.sh`'s link roster spells the NAME.
///
/// ⚠ **`vike-cli` keeps its prefix, as a name AND as a feature.** It is a separately shipped
/// binary as well as a row here, and stripping it to `cli` would rename the link the pre-flight
/// below runs.
const TOOLS: &[Tool] = &[
    #[cfg(feature = "study-cli")]
    Tool { name: "study", main: study_main, summary: "run a study FILE over the hist store" },
    // ⚠ The VERB is `report`; the FEATURE stays `tearsheet`. Two different names on purpose, and
    // this is the one row where they diverge.
    //
    // ⚠ RETIRED 2026-11-18 (owner): the row folded a live journal, and now answers every invocation
    // with `vike_cli::cmd::report::JOURNAL_DOORS_RETIRED` on the usage rung (`report_main`). It stays
    // a row so a unit or a link still naming it meets the new spelling rather than the tool list.
    #[cfg(feature = "tearsheet")]
    Tool {
        name: "report",
        main: report_main,
        summary: "RETIRED: refused, naming `vike-cli report account:<venue>/<LABEL>`",
    },
    #[cfg(feature = "backtesthub")]
    Tool { name: "backtesthub", main: backtesthub_main, summary: "run a backtest and mint a run" },
    // ⚠ **`recorder` IS GONE FROM THIS TABLE and its work is here** — ruling 10
    // (`docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` §0.5) merged the
    // recorder daemon into the data server, so one process owns venue connections, the store and
    // serving. The tape is now `vike-backend datahub --record <profile>`; `vike-backend recorder`
    // is `ToolRoute::Usage(2)` — `crates/vike-backend/src/lib.rs`'s `resolve` matches by EXACT string with no
    // aliases, so a unit still naming it prints this dispatcher's tool list and exits 2 rather than
    // failing to exec. `docs/ops/recorder-deploy.md` carries the operator's flip, in order, so the
    // recorder never stops recording.
    #[cfg(feature = "vike-datahub")]
    Tool {
        name: "datahub",
        main: datahub_main,
        summary: "serve the hist store, and with --record fill it from the venues",
    },
    // ⚠ The GATEWAY daemon (decision 0164, wave 1): the one process that owns every venue connection,
    // and the data daemon above reaches a venue only through it. Its OWN standalone binary
    // (`crates/vike-gatewayhub/src/bin/vike-gatewayhub.rs`) still exists and works unchanged — this
    // row is a SECOND composition root over the same `vike_gatewayhub::gatewayhub_cli::run`, which is
    // what `deploy/vike-gatewayhub.service` starts as `vike-backend gatewayhub`. The verb is
    // `gatewayhub`, not `vike-gatewayhub` (the prefix is redundant with the binary), and it differs
    // from the FEATURE (`vike-gatewayhub`, which names the optional dependency) exactly as `datahub`
    // differs from `vike-datahub`: do not "align" them.
    #[cfg(feature = "vike-gatewayhub")]
    Tool {
        name: "gatewayhub",
        main: gatewayhub_main,
        summary: "the gateway daemon: the one process that owns every venue connection",
    },
    // ⚠ This is the one verb whose rename can take the live order-signing daemon down: it is the
    // `ExecStart=` of `deploy/vike-tradehub.service` and of the supervisor's tradehub row
    // (`deploy/docker/entrypoint.sh`'s `SERVER_RULES`); the retired root deploy helper refused a box
    // whose installed unit named a verb the release no longer answers. It was `trade` until
    // 2026-11-06, the owner's word for the daemon is the hub's own name, and `vike-cli trade` (the
    // client) keeps its.
    //
    // ⚠ The CRATE stays `vike-tradehub` and so does the FEATURE. Renaming a crate is a different
    // change with a different blast radius, and the verb is what an operator types.
    #[cfg(feature = "vike-tradehub")]
    Tool { name: "tradehub", main: tradehub_main, summary: "the live trading daemon" },
    // ⚠ Same feature as `tradehub`, and NOT a daemon. See `catalog_main`.
    #[cfg(feature = "vike-tradehub")]
    Tool {
        name: "catalog",
        main: catalog_main,
        summary: "list a credentialed venue's instruments with this box's own credentials",
    },
    // ⚠ Same feature as `tradehub` and `catalog`, same reason, and NOT a daemon. See `venues_main`.
    #[cfg(feature = "vike-tradehub")]
    Tool {
        name: "venues",
        main: venues_main,
        summary: "which venues and accounts this box would mount, and why the rest do not run",
    },
    // ⚠ Same feature as `tradehub`, `catalog` and `venues`, and NOT a daemon. See `notify_main`.
    #[cfg(feature = "vike-tradehub")]
    Tool {
        name: "notify",
        main: notify_main,
        summary: "the OnFailure= pager: report a failed systemd unit to Telegram",
    },
    // The Telegram control channel (decision 0256): `deploy/vike-telegram.service`'s `ExecStart=`, a
    // client of the tradehub's control wire in its own process. The verb is `telegram`, the feature
    // `vike-telegram` (the optional dependency), as `tradehub` is to `vike-tradehub`.
    #[cfg(feature = "vike-telegram")]
    Tool {
        name: "telegram",
        main: telegram_main,
        summary: "the Telegram control channel: a client of the tradehub's control wire",
    },
    #[cfg(feature = "vike-cli")]
    Tool {
        name: "vike-cli",
        main: vike_cli_main,
        summary: "the operator CLI — config, secrets, trade",
    },
    // ⚠ The daemon that runs `cargo` against an authenticated caller's Rust strategy source
    // (`docs/superpowers/specs/2026-09-21-runtime-loaded-rust-strategies-design.md`, "The builder
    // service"). Its OWN standalone binary (`crates/vike-strategy-builder/src/bin/
    // vike-strategy-builder.rs`) still exists and still works unchanged — this row is a SECOND
    // composition root over the same `vike_strategy_builder::builder::run`, not a replacement.
    #[cfg(feature = "vike-strategy-builder")]
    Tool {
        name: "strategy-builder",
        main: strategy_builder_main,
        summary: "compile an authenticated caller's Rust strategy so a backtest can load it",
    },
    // ⚠ NOT a daemon, and nobody types it: the process a hub SPAWNS to run one Rust strategy file jailed
    // (`crates/vike-strategy-sandbox/src/lib.rs`'s module doc carries the verbs and the descriptors). It is
    // the only process that loads a strategy file, which is what keeps every hub free of the loader.
    #[cfg(feature = "vike-strategy-sandbox")]
    Tool {
        name: "strategy-sandbox",
        main: strategy_sandbox_main,
        summary: "run one Rust strategy file jailed, for the hub that spawned it",
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
fn vike_cli_main(_context: &ToolContext<'_>, args: &[String]) -> ExitCode {
    vike_cli::run(args.iter().cloned())
}

/// ⚠ Hands over its `env` (where the settings database is, whose one row is the key) and its `args`
/// (the four plain settings, `vike_strategy_builder::builder::read_args`), the same two its
/// standalone binary passes.
/// `vike_strategy_builder::builder::run` is the ENTIRE sequence: every refusal, every
/// default, the socket bind and the (never-returning, on success) serve loop, in one shared
/// function this dispatcher and the crate's own `vike-strategy-builder` binary both call — see
/// that function's doc for why the sequence lives there rather than being copied here. This row
/// exists so `--features full` (what `.github/workflows/release.yml` builds) carries this daemon
/// too; nothing about what it DOES differs from running the standalone binary.
#[cfg(feature = "vike-strategy-builder")]
fn strategy_builder_main(context: &ToolContext<'_>, args: &[String]) -> ExitCode {
    // The key is read by `run` from the settings database `context.env` locates — this dispatcher opens
    // no store of its own (`vike_strategy_builder::builder::resolve_keys` says how).
    // `reentry` is this verb: the jail probe re-runs this program as `<program> strategy-builder
    // --jail-probe`, the `vike-backend <verb>` shape `vike::resolve` routes.
    vike_strategy_builder::builder::run(context.env, args, &[context.invoked_as])
}

/// The sandboxed strategy process. It takes no context: the parent spawns it with a CLEARED environment and
/// working directory `/`, so there is nothing here to hand over. The backtest verb's body is the backtesthub's
/// own (`vike_backtesthub::strategy_sandbox::serve_child`), handed in as a function because the sandbox crate
/// and the backtesthub sit on one layer and the backtesthub must not link the loader. This binary's build is
/// handed in too: the backtest and live verbs state it to their parent first (the same-build check), and a
/// library links no `vike-buildinfo`.
#[cfg(feature = "vike-strategy-sandbox")]
fn strategy_sandbox_main(_context: &ToolContext<'_>, args: &[String]) -> ExitCode {
    vike_strategy_sandbox::run(
        args,
        vike_backtesthub::strategy_sandbox::serve_child,
        vike_backtesthub::study_sandbox::serve_child,
        &vike_buildinfo::summary(),
    )
}

/// ⚠ The daemon, and the one tool where the ORDER of what happens next is a safety property.
/// `tradehub_cli::run` installs the signal handlers as its first statement, before anything can
/// mount a venue or place an order, and `crates/vike-ops/tests/container_deploy/graceful_stop_pin.rs` checks that
/// positionally. **This dispatcher must add nothing before that call** — an unhandled window
/// already exists before any `main`'s first statement, but anything added here would widen it.
///
/// The builder a `.rs` strategy file is built by before its sandboxed mount (plan 2b §3) is HANDED to
/// `run` (it installs it after the handlers): the builder's client is a crate the tradehub cannot name
/// (both layer 65). The artifact's path is the builder's answer (`sdk-N/<folder>/<sha>.so` under the
/// builds directory, `vike_strategy_builder::client::Built::artifact`), handed over with the SDK it was
/// built against (decision 0296). Nothing is read until a mount needs a build: the key is read from the
/// settings database `context.env` locates.
#[cfg(feature = "vike-tradehub")]
fn tradehub_main(context: &ToolContext<'_>, args: &[String]) -> ExitCode {
    let env = context.env.clone();
    let builder: vike_tradehub::config::FileBuilder = Box::new(move |builds, path, text| {
        use vike_strategy_builder::client::{BuildFileError, BuildRequestError};
        use vike_tradehub::config::OpenRefusal;
        // The compiler's verdict is the file's and final; anything else is the builder not answering, asked again.
        // An SDK the file cannot build against (retired, another boundary, not installed) is refused
        // as a compile is: the builder's `BuildErr`, its words carrying the fix.
        // Each refusal is handed on with its one id (the builder's, a compile's own), which the mount row shows.
        let built =
            // No override: a daemon mount follows its file (decision 0296), the `[strategy] sdk` it pins.
            vike_strategy_builder::client::build_file(&env, path, text, None).map_err(|e| {
                let refusal = e.carried();
                match e {
                    BuildFileError::Request(BuildRequestError::Compile { .. }) => {
                        OpenRefusal::Refused(refusal)
                    }
                    _ => OpenRefusal::BuilderUnanswered(refusal),
                }
            })?;
        // A build of another SDK than the file pins is never run (`Built::of_the_sdk`): refused for good.
        let pinned = vike_strategy_builder::render::pin_of(text);
        built.of_the_sdk(pinned).map_err(OpenRefusal::Refused)?;
        Ok((built.artifact(builds), built.sdk))
    });
    vike_tradehub::tradehub_cli::run(context.env, context.cwd, args, Some(builder))
}

/// The Telegram control channel, handed exactly what the daemons are: the environment this dispatcher
/// swept ONCE, the working directory it read ONCE, and the argv tail `resolve` already cut.
/// `vike_telegram::telegram_cli::run` installs its stop handlers before it boots, so this adds nothing
/// before that call.
#[cfg(feature = "vike-telegram")]
fn telegram_main(context: &ToolContext<'_>, args: &[String]) -> ExitCode {
    vike_telegram::telegram_cli::run(context.env, context.cwd, args)
}

/// ⚠ **A READ-ONLY REPORT behind the daemon's feature, and it starts no daemon and mounts nothing.**
///
/// `vike-backend venues` answers *"I set `live` and it is still on paper — why?"*. It shares
/// `vike-tradehub`'s feature for `catalog_main`'s reason below, one rung sharper: its `EFFECTIVE`
/// column is `vike_tradehub_core::venue_account_arming`'s answer, and that function reaches thirteen venue
/// crates because each exchange's tier rule is its own. `vike-cli` — where an operator might look
/// for it first — is held transport-free by `crates/vike-boot/tests/dependency_floor.rs`, the same
/// gate that refuses `catalog` there, so this binary is the only place the screen can be built at
/// all. It installs no signal handler, opens no venue socket and can place no order.
#[cfg(feature = "vike-tradehub")]
fn venues_main(context: &ToolContext<'_>, args: &[String]) -> ExitCode {
    vike_tradehub::venues_cli::run(context.env, context.cwd, args)
}

/// ⚠ **The `OnFailure=` pager — a ONE-SHOT COMMAND behind the daemon's feature, starting no
/// daemon.**
///
/// `vike-backend notify <unit>` is what `deploy/vike-notify@.service` runs when a production unit
/// fails. It shares `vike-tradehub`'s feature for `venues_main`'s reason: `vike-cli` is held
/// transport-free (`scripts/ci_feature_suite.sh`'s `light-consumers` arm refuses `ureq`/`rustls`
/// in its normal tree) and a Telegram POST is HTTPS, while that crate already links the alerting
/// transport and the credential store. It installs no signal handler, opens no listening socket
/// and can place no order; its boot, its scoped credential read and its state all happen inside
/// `vike_tradehub::notify_cli::run`.
#[cfg(feature = "vike-tradehub")]
fn notify_main(context: &ToolContext<'_>, args: &[String]) -> ExitCode {
    vike_tradehub::notify_cli::run(context.env, context.cwd, args)
}

/// ⚠ **A ONE-SHOT COMMAND behind the same feature as the daemon above, and it starts no daemon.**
///
/// `vike-backend catalog` lists a CREDENTIALED venue (alpaca / oanda / ctrader) with THIS BOX'S own
/// credentials — `docs/decisions/0066`'s decision 9. It shares `vike-tradehub`'s feature because it
/// shares that crate's bridge edges and its credential-store read, and for no other reason: it
/// mounts nothing, installs no signal handler and can place no order.
///
/// ⚠ It is a SEPARATE ROW rather than a subcommand of `trade`, deliberately. That verb's summary is
/// "the live trading daemon" and it is the `ExecStart=` of two tracked units; folding a one-shot
/// command into its argv would put a parse ahead of the signal handlers
/// `crates/vike-ops/tests/container_deploy/graceful_stop_pin.rs` checks positionally.
#[cfg(feature = "vike-tradehub")]
fn catalog_main(context: &ToolContext<'_>, args: &[String]) -> ExitCode {
    vike_tradehub::catalog_cli::run(context.env, context.cwd, args)
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
fn datahub_main(context: &ToolContext<'_>, args: &[String]) -> ExitCode {
    vike_datahub::datahub_cli::run(context.env, context.cwd, args)
}

/// ⚠ The gateway daemon, handed exactly what the data daemon above is handed: the environment this
/// dispatcher swept ONCE and the working directory it read ONCE, and the argv tail `resolve` already
/// cut (no program name, no verb). `gatewayhub_cli::run` installs the stop handlers as the first
/// thing after it has answered `--help`, then boots (`vike_boot::boot`) and logs on its own, so this
/// dispatcher adds nothing before that call and must not — the same rule as the trading daemon's row.
#[cfg(feature = "vike-gatewayhub")]
fn gatewayhub_main(context: &ToolContext<'_>, args: &[String]) -> ExitCode {
    vike_gatewayhub::gatewayhub_cli::run(context.env, context.cwd, args)
}

/// ⚠ Supplies the CLOCK as well as the environment. `crates/vike-ops/tests/architecture/clock_pin.rs`'s
/// `CLOCK_PIN` keeps `SystemTime::now()` out of the library tree, so `backtest_cli::run` takes the
/// timestamp as a parameter and a composition root reads it. This dispatcher IS that root.
///
/// ⚠ A clock set before the epoch answers with a NEGATIVE second rather than `0` — zero is a real
/// instant, and a failure wearing a valid value is how a run manifest starts lying about when it
/// happened. That is `vike_model::time::clock::now_unix_secs`'s behaviour, shared with the shipped
/// bin's own reader (the two were one copy-pasted function until it moved there).
#[cfg(feature = "backtesthub")]
fn backtesthub_main(context: &ToolContext<'_>, args: &[String]) -> ExitCode {
    // ⚠ THE BUILD STAMP (`vike_runs::BuildStamp`). This dispatcher is a composition root
    // that DOES depend on `vike-buildinfo` — it already prints `version_line` for its own
    // `--version` — so it can answer the question `crates/vike-backtesthub`'s standalone bin is exempted from asking:
    // that crate's build script would rerun on every HEAD move and rebuild the simulator and
    // everything above it. The library declares a
    // dependency-free seam and the root fills it.
    //
    // ⚠ The sha is the literal `UNKNOWN` when it could not be resolved (no git, a source tarball, a
    // shallow clone), and the literal `FROZEN` in a CI or lane DEBUG build under the freeze switch.
    // Both convert to `None` rather than being written through, because a manifest field a reader
    // takes for a commit must not hold either word — `vike_backend::manifest_git_sha` is the filter,
    // and its tests hold this call site to it.
    //
    // ⚠ `summary` must OUTLIVE `build`, which borrows it — declared first, because locals drop in
    // reverse declaration order.
    let summary = vike_buildinfo::summary();
    let build = vike_runs::BuildStamp { git_sha: build_commit(), summary: Some(&summary) };
    // The builder a `.rs` strategy file is built by before its sandboxed run (plan 2b §3): its client is
    // a crate the backtesthub cannot name (both layer 65). Nothing is read here: the key is read from
    // the settings database `context.env` locates when a run first needs a build.
    let env = context.env.clone();
    // `sdk` is a backtest profile's `[strategy] sdk` (or a Studio's pick): the builder's override
    // (`Request::Build`'s `sdk`), over the file's own pin.
    vike_backtesthub::strategy_sandbox::use_builder(Box::new(move |path, text, sdk| {
        let built = vike_strategy_builder::client::build_file(&env, path, text, sdk)
            .map_err(|e| e.carried())?;
        // A build of another SDK than the run asks for (or, unasked, the file pins) is never run.
        built.of_the_sdk(sdk.or_else(|| vike_strategy_builder::render::pin_of(text)))?;
        Ok(vike_backtesthub::strategy_sandbox::BuiltFile {
            artifact: built.relative_artifact(),
            sha256: built.sha,
        })
    }));
    // This binary's build, the first frame of every sandboxed child's pipe (a library links no `vike-buildinfo`).
    vike_backtesthub::strategy_sandbox::use_build(summary.clone());
    use_study_builder(context);
    // The strategy SDKs the Studio's SDK source verbs list and read (decision 0296, verdict 3): read with
    // the SDK vocabulary no hub may link, handed in as a function of the project the daemon's own walk
    // finds. Nothing is read here.
    vike_backtesthub::sdk_source::use_reader(Box::new(sdks_under));
    vike_backtesthub::backtest_cli::run(
        context.env,
        args,
        &vike_model::time::clock::now_unix_secs,
        Some(build),
    )
}

/// The RETIRED journal door (owner, 2026-11-18): every invocation, `--help` and `--version`
/// included, is refused with the ONE line `vike-cli report --node` answers too, on the usage rung.
/// It reads nothing: no journal, no environment, no store.
#[cfg(feature = "tearsheet")]
fn report_main(_context: &ToolContext<'_>, _args: &[String]) -> ExitCode {
    eprintln!("vike-backend report: {}", vike_cli::cmd::report::JOURNAL_DOORS_RETIRED);
    ExitCode::from(2)
}

/// ⚠ **This took NO context until 2026-09-23, on the stated ground that `study` "reads no
/// environment variable at all — it takes eight `--flags` and nothing else".** That stopped being
/// true when the study started routing its history
/// (`docs/decisions/0084-only-the-datahub-touches-the-store.md`): the peer comes from
/// the `config.datahub_addr` row and the credential store, both under `$VIKE_SETTINGS_DIR`. It is
/// the process's ONE
/// sweep that is handed over, not a second read — which is the whole reason `ToolContext` carries it.
#[cfg(feature = "study-cli")]
fn study_main(context: &ToolContext<'_>, args: &[String]) -> ExitCode {
    // A study's backtest of a strategy file runs in a sandboxed child too: this binary's build goes first.
    vike_backtesthub::strategy_sandbox::use_build(vike_buildinfo::summary());
    use_study_builder(context);
    vike_backtesthub::study_cli::run(context.env, args)
}

/// This build's commit for a manifest or a dev SDK row: the ONE place `main.rs` reads `GIT_SHA`, through
/// `vike_backend::manifest_git_sha`'s filter (`UNKNOWN`/`FROZEN` become `None`), so no call site filters inline.
#[cfg(feature = "backtesthub")]
fn build_commit() -> Option<&'static str> {
    vike_backend::manifest_git_sha(vike_buildinfo::GIT_SHA)
}

/// The builder a research study FILE is built by before its sandboxed run (owner's rule, 2026-11-15: a
/// study is a FILE, built like a strategy file): its client is a crate the backtesthub cannot name (both
/// layer 65). Nothing is read here: the key is read from the settings database `context.env` locates when a
/// study first needs a build.
#[cfg(any(feature = "backtesthub", feature = "study-cli"))]
fn use_study_builder(context: &ToolContext<'_>) {
    let env = context.env.clone();
    vike_backtesthub::study_sandbox::use_builder(Box::new(move |name, files| {
        vike_strategy_builder::client::build_study(&env, name, files)
            .map(|built| built.relative_artifact())
            .map_err(|e| e.carried())
    }));
}

/// **The strategy SDKs under `project`**, as the backtesthub's Studio verbs list and read them (decision
/// 0296, verdict 3; `crates/vike-backtesthub/src/sdk_source.rs`'s module doc says why this root reads them):
/// a git checkout is "SDK dev", its live tree (`sdk-dev`, this binary's commit and interface version, the one
/// SDK a file builds against there, whatever it pins); else every installed `sdk-N`, newest first, usable
/// or not by the vocabulary's one rule (`vike_rust_strategy::sdk::usable`, against the newest). A snapshot
/// whose marker cannot be read is listed, unusable, with the marker's own refusal.
#[cfg(feature = "backtesthub")]
fn sdks_under(
    project: &std::path::Path,
) -> Result<vike_backtesthub::sdk_source::ServerSdks, String> {
    use vike_backtesthub::sdk_source::{SdkOnServer, ServerSdks};
    use vike_backtesthub_client::sdk_source::WireSdk;
    use vike_rust_strategy::sdk::{self, Sdk, SdkMarker};

    if sdk::is_dev_checkout(project) {
        let dev = WireSdk {
            name: Sdk::Dev.directory_name(),
            number: None,
            commit: build_commit().unwrap_or_default().to_string(),
            abi_version: Some(vike_rust_strategy::abi::ABI_VERSION),
            usable: true,
            why_unusable: String::new(),
        };
        let dir = project.to_path_buf();
        return Ok(ServerSdks { root: dir.clone(), sdks: vec![SdkOnServer { sdk: dev, dir }] });
    }
    let root = project.join(sdk::SDK_SOURCE_DIR);
    let numbers = sdk::installed(&root)?;
    let newest = numbers.last().copied().unwrap_or_default();
    // A malformed adapter is no adapter for its SDK alone, never the whole list.
    let adapters = sdk::host_adapters(&root).adapters;
    let sdks = numbers
        .iter()
        .rev()
        .map(|&number| {
            let dir = sdk::snapshot_dir(&root, number);
            let marker = SdkMarker::read(&dir);
            let unusable = match &marker {
                Ok(marker) => {
                    sdk::usable(marker, &dir, newest, &adapters).err().map(|why| why.to_string())
                }
                Err(why) => Some(why.clone()),
            };
            let marker = marker.ok();
            let sdk = WireSdk {
                name: Sdk::Number(number).directory_name(),
                number: Some(number),
                commit: marker.as_ref().map(|m| m.commit.clone()).unwrap_or_default(),
                abi_version: marker.as_ref().map(|m| m.abi_version),
                usable: unusable.is_none(),
                why_unusable: unusable.unwrap_or_default(),
            };
            SdkOnServer { sdk, dir }
        })
        .collect();
    Ok(ServerSdks { root, sdks })
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().collect();
    let env: HashMap<String, String> = std::env::vars().collect();
    let cwd = std::env::current_dir().ok();
    match resolve(TOOLS, &argv) {
        ToolRoute::Tool { index, args } => {
            let tool = &TOOLS[index];
            let context = ToolContext { env: &env, cwd: cwd.as_deref(), invoked_as: tool.name };
            dispatch(tool, &context, args)
        }
        ToolRoute::Usage(code) => {
            // Code 0 means the user ASKED, so it belongs on stdout where `| less` can see it; a
            // refusal belongs on stderr. Same split `vike-cli` already makes.
            if code == 0 {
                print!("{}", usage(TOOLS));
            } else {
                eprint!("{}", usage(TOOLS));
            }
            ExitCode::from(code)
        }
        ToolRoute::Version => {
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

/// `sdks_under` on planted projects: what the backtesthub's `ListSdks` lists comes from here.
#[cfg(all(test, feature = "backtesthub"))]
mod sdk_reader_tests {
    use std::path::Path;

    use vike_rust_strategy::sdk::{BOUNDARY_HASH, SDK_SOURCE_DIR, SdkMarker, SdkSourceHashes};

    use super::sdks_under;

    /// Install a marker for SDK `number` cut with the boundary `boundary_hash` under `project`.
    fn plant(project: &Path, number: u32, boundary_hash: &str) {
        let dir = project.join(SDK_SOURCE_DIR).join(format!("sdk-{number}"));
        std::fs::create_dir_all(dir.join("crates/vike-tactics/src")).expect("mkdir");
        let hash = || "0123456789abcdef".to_string();
        let marker = SdkMarker {
            number,
            commit: format!("c0ffee{number}"),
            abi_version: 14,
            boundary_hash: boundary_hash.to_string(),
            source_hash: SdkSourceHashes { strategy: hash(), onnx: hash(), study: hash() },
        };
        marker.write(&dir).expect("the marker");
    }

    /// Every installed SDK, newest first, each with its marker's commit and interface version: the newest
    /// usable, one cut with another boundary refused in the vocabulary's words, one whose marker does
    /// not parse listed unusable with the marker's refusal. Each row's folder is its snapshot.
    #[test]
    fn every_installed_sdk_is_listed_newest_first_with_its_usability() {
        let project = tempfile::tempdir().expect("a project");
        plant(project.path(), 5, BOUNDARY_HASH);
        plant(project.path(), 4, "ffffffffffffffff");
        let broken = project.path().join(SDK_SOURCE_DIR).join("sdk-2");
        std::fs::create_dir_all(&broken).expect("mkdir");
        std::fs::write(broken.join("SDK"), "number = \"two\"\n").expect("a broken marker");
        let found = sdks_under(project.path()).expect("read");
        assert_eq!(found.root, project.path().join(SDK_SOURCE_DIR));
        let rows: Vec<_> = found
            .sdks
            .iter()
            .map(|s| {
                let w = &s.sdk;
                (w.name.as_str(), w.number, w.commit.as_str(), w.abi_version, w.usable)
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("sdk-5", Some(5), "c0ffee5", Some(14), true),
                ("sdk-4", Some(4), "c0ffee4", Some(14), false),
                ("sdk-2", Some(2), "", None, false),
            ]
        );
        assert!(found.sdks[0].sdk.why_unusable.is_empty());
        assert!(found.sdks[1].sdk.why_unusable.contains("another strategy boundary"));
        assert!(found.sdks[2].sdk.why_unusable.contains("does not parse"));
        assert_eq!(found.sdks[0].dir, project.path().join(SDK_SOURCE_DIR).join("sdk-5"));
    }

    /// A project with no SDK installed lists none; a git checkout is "SDK dev", its own tree the one SDK.
    #[test]
    fn no_sdk_lists_none_and_a_checkout_is_sdk_dev() {
        let project = tempfile::tempdir().expect("a project");
        assert!(sdks_under(project.path()).expect("read").sdks.is_empty());
        std::fs::create_dir_all(project.path().join(".git")).expect("a checkout");
        plant(project.path(), 5, BOUNDARY_HASH);
        let found = sdks_under(project.path()).expect("read");
        assert_eq!(found.sdks.len(), 1, "a checkout builds every file against its own tree");
        let dev = &found.sdks[0];
        assert_eq!(
            (dev.sdk.name.as_str(), dev.sdk.number, dev.sdk.usable),
            ("sdk-dev", None, true)
        );
        assert_eq!(dev.dir, project.path());
    }
}
