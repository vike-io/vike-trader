//! `vike-cli research study` — ask the backend to run a compiled study over the hist store IT
//! holds.
//!
//! ⚠ **This was a TOP-LEVEL verb until ruling 1 of the 2026-09-13 owner rulings on
//! `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md`.** A study fits a MODEL — a
//! LightGBM child process per fold, a fitted artifact rather than a PnL curve — so it is not an
//! operation of the backtest plane and never was: it is the first sub-verb of a fourth plane,
//! `crate::cmd::research`. It moved INWARD rather than away: `vike-cli study` answers with
//! `vike-cli research study` through `crate::RETIRED_COMMANDS`, and the capability stays
//! discoverable both as a `COMMANDS` row (`research`) and in that plane's `SUBCOMMANDS` and
//! `USAGE`. Nothing about the grammar, the ladder, the negotiation or the messages changed with
//! the move; only the prefix each message wears, and who calls [`run`].
//!
//! ⚠ The refusal below is no longer open-ended. Ruling 1 put the missing WIRE REQUEST in scope
//! for stage 7 — one refusing verb was tolerable, a refusing PLANE is not — so
//! `vike_datahub_client::FEATURE_STUDY` is an anchor with a date on it rather than a placeholder.
//!
//! The second half ruling 16 of
//! `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` found missing: the
//! backend has `study` (`crates/vike-studio-core/src/study_cli.rs`, reached as `vike-backend
//! study`) and the client had nothing. The argument for a client half is the one that put
//! `vike-cli backtest` on the wire in the first place — compute-to-data. A study opens a
//! `DataFusionHist`, folds a strategy over a window of it and drives a LightGBM child process per
//! fold; the data and the CPU are on the backend box, and a laptop that shipped the history to
//! itself would be doing the one thing this whole design exists to avoid.
//!
//! # ⚠ WHICH DAEMON THIS ASKS — the COMPUTE one, not the data server and not the trading node
//!
//! A study RUNS an engine, so it is a COMPUTE verb, and ruling 7 of that same spec says in a table
//! who serves those: the seven `Run*`/`ListStrategies` verbs leave `vike-backend datahub` for
//! **`vike-backend backtesthub --addr <bind>`**. So this verb dials the backtesthub, and its
//! address ladder is that daemon's — `--addr <v>` → `config.backtest_addr` →
//! [`vike_config::DEFAULT_BACKTEST_ADDR`] — folded in [`resolve_addr`] and nowhere else.
//!
//! ⚠ **The last rung is `127.0.0.1:7880`, and the digits are load-bearing.** `7879` is the LIVE
//! ORDER-SIGNING daemon (`config.tradehub_addr`; `ss -ltnp` on the production box on 2026-09-10:
//! `127.0.0.1:7879 users:(("vike-backend",pid=…))`), and `7878` is the data server ruling 7 is
//! taking compute away FROM. A default that named either would aim this verb at a process that
//! does not serve it — one of them the process that signs orders. `vike_config`'s
//! `DEFAULT_BACKTEST_ADDR` is the ONE spelling of the number, shared with the daemon that binds it.
//!
//! # ⚠ THE REQUEST EXISTS since stage 7, and this section said the opposite
//!
//! It was headed *"THE SCOPE DECISION, and why this file sends no request"*, and the reasoning was
//! sound while it held: `vike_datahub_client::proto`'s `Request` had `RunBacktest`/`RunSlice`/
//! `RunSweep`/`RunWalkforward`, every one of them "run this strategy over this window and rank the
//! result", and a study is a different operation — a named entry in the compiled study registry, a
//! recipe with a `[learner]` table, a fitted model per fold and a minted run directory. So the verb
//! needed a NEW wire request, and ruling 7's split was in flight when this file was written.
//!
//! That deferral is SPENT. `Request::RunStudy` carries the study name, the recipe's TEXT and the
//! window; `Response::StudyReport` brings the minted run back as JSON; and `run` below SENDS.
//!
//! # ⚠ What an invocation does — and the two refusals that REMAIN
//!
//! The capability is still negotiated, but every backtesthub advertises `FEATURE_STUDY` now: the
//! daemon lives in `vike-backtesthub` (above `vike-studio-core`) and calls the study runner directly, so
//! `vike-backend backtesthub --addr` and `cargo run -p vike-backtesthub --bin backtesthub` both serve a study.
//! (Until the daemon moved out of `vike-backtest` the runner was handed down by the one composition
//! root that could reach it, and a bare `backtesthub` bin refused the verb by name.)
//!
//! 1. **The peer answers and does not advertise the capability** — [`no_capability_lines`], which
//!    names the missing capability, says nothing was sent, and carries the `vike-backend study`
//!    invocation with this run's own arguments filled in. It is the answer against the DATA server,
//!    which advertises no `study` and never will, and against an older backend that predates the
//!    direct call. `crates/vike-cli/tests/study_report_refusal_cli.rs` drives the shipped binary against
//!    a REAL `vike_datahub::serve` and asserts both the words and that the server counted no frame
//!    after the handshake.
//! 2. **The dial fails** — [`connect_failure_lines`], which names the address, says which half
//!    failed, and carries the same invocation anyway, because an operator who cannot reach a
//!    backtesthub needs the command that works even more than one who reached a peer.
//!
//! Either way the operator ends with the escape hatch spelled correctly.
//!
//! ⚠ **`client_too_old_lines` is DELETED, and that is a consequence rather than an oversight.** It
//! fired for "the backend advertises the capability, this client predates the request variant" —
//! and this client IS the variant. A message that cannot fire is the `Policy::max_total_exposure`
//! shape: positive confirmation of something false.
//!
//! # Why the recipe is read HERE, and the window is not parsed here
//!
//! The recipe is a LOCAL file whose TEXT crosses the wire, exactly as `backtest --profile`'s does
//! and for the identical reason (`crate::cmd::backtest`'s module doc argues it): the backend is
//! typically a different machine, so a PATH would be resolved against ITS filesystem and the
//! recipe an author is editing would be invisible to the run they just started. Reading it now
//! also fails a typo at the door instead of after a round trip.
//!
//! `--from`/`--to` travel as the operator's own strings and are validated only for being
//! non-empty. The backend already owns that grammar — `YYYY-MM-DD`, `YYYY-MM-DDTHH` or bare unix
//! SECONDS, in `study_cli`'s `boundary_ms` — and a second parser here would be a second answer to
//! "what does this string mean", which is the failure class this workspace re-learns most often.
//! The server parses and validates, the same division `Request::RunBacktest` draws over profile
//! TOML.
//!
//! # What is / is not tested
//!
//! The grammar (`parse`), the address ladder ([`resolve_addr`]), the two negotiation messages
//! ([`no_capability_lines`]), the backend fallback
//! ([`backend_fallback`]) and both halves of the dial-failure mapping ([`connect_failure_lines`]
//! for the words, [`connect_failure_exit`] for the rung) are PURE and unit-tested below. The
//! NEGOTIATION itself is not pure and is not left to a unit test either:
//! `crates/vike-cli/tests/study_report_refusal_cli.rs` runs the shipped binary against a real
//! server of this protocol on a loopback port. ⚠ What has no test HERE is a SUCCESSFUL study, and
//! that is a placement rather than a hole: the success path needs a mounted runner and a seeded
//! store, so it is proven where those live —
//! `crates/vike-backtesthub/tests/backtest_plane.rs`'s `every_daemon_advertises_the_study_capability` and `a_study_with_an_unparsable_recipe_is_refused_by_the_real_runner_and_the_connection_survives`
//! over the real daemon, and `crates/vike-datahub-client/tests/study_negotiation.rs` for the
//! client's own send-vs-refuse decision.

use std::io;
use std::path::Path;
use std::process::ExitCode;

use vike_datahub_client::{DatahubClient, FEATURE_STUDY, WireStudy};
use vike_hub_proto::auth::{HubKeys, Scope};

use crate::cmd::args;
// `backtest` and `study` dial the SAME backtesthub on the SAME `config.backtest_addr` setting,
// so the fold lives in the verb the setting is NAMED for and this file is a CALLER. There is
// deliberately no `pub use` shim — the no-shim-on-a-move convention.
//
// ⚠ That is NOT the same as "these are the only two verbs that dial it", and this comment used to
// read "ONE ladder, TWO verbs", which somebody grepping the phrase would find first and believe.
// `walkforward` and `mcp`'s compute tools dial the same daemon and read the setting from nowhere.
// `crates/vike-cli/src/cmd/backtest.rs`'s `resolve_addr` carries the whole property, and carries no
// count, deliberately — read it there rather than restating it here.
use crate::cmd::backtest::resolve_addr;
use crate::exit::Exit;

/// The program name this verb's refusals are printed under (`research` owns the parse, so this is
/// the plane's program name plus the sub-verb).
const BIN: &str = "vike-cli research study";

// The prose half of `research study --help`; the usage line and the option table are `clap`'s,
// derived from [`StudyCmd`].
const LONG_ABOUT: &str = "\
Ask the backend to run one COMPILED study over the hist store IT holds, and to leave a run
behind there. The store, the CPU and the pinned LightGBM trainer are the backend's; only the
recipe text and the window cross the wire.

⚠ Every backtesthub serves this — `vike-backend backtesthub --addr` and the `backtesthub` bin
(`cargo run -p vike-backtesthub --bin backtesthub`) alike, since the daemon calls the study runner
directly. A run that ends at the dial, or at a peer that does not advertise it (the data server,
or an older backend), names `vike-backend study` — the same work, on the box that holds the
store, with these flags already filled in — and nothing goes on the wire in either case.

The store root and the LightGBM binary are NOT flags here: both name paths on the backend's
box, which this side cannot see. They are that daemon's configuration.";

/// `research study`'s arguments, as `clap` reads them. The four window/recipe flags are REQUIRED
/// (a blank value is refused by the shared reader, for every flag), and the two retired flags are
/// declared hidden so that they are answered by NAME rather than as typos — see
/// [`StudyCmd::into_args`].
#[derive(clap::Args, Debug)]
#[command(
    about = "Ask the backend to run one COMPILED study over the hist store IT holds, and leave a \
             run behind there",
    long_about = LONG_ABOUT
)]
pub(crate) struct StudyCmd {
    #[arg(
        long,
        value_name = "NAME",
        help = "the study's folder name under user_data/research/studies/rust/"
    )]
    study: String,
    #[arg(
        long,
        value_name = "FILE",
        help = "its .toml recipe, on THIS machine — its TEXT is shipped, never its path"
    )]
    recipe: String,
    #[arg(
        long,
        value_name = "WHEN",
        help = "YYYY-MM-DD, YYYY-MM-DDTHH, or bare unix SECONDS (the backend parses them)"
    )]
    from: String,
    #[arg(long, value_name = "WHEN", help = "the window's end, in the same spellings as --from")]
    to: String,
    #[arg(
        long,
        value_name = "HOST:PORT",
        help = "the backtesthub (`vike-backend backtesthub --addr`, where ruling 7 puts every verb \
                that runs an engine). Resolved as --addr, then the `config.backtest_addr` \
                setting, then 127.0.0.1:7880 — NOT 7879, which is the live order-signing daemon, \
                and not 7878, which is the data server"
    )]
    addr: Option<String>,
    #[arg(
        long,
        help = "print the backend's own run document verbatim instead of the rendered lines"
    )]
    json: bool,
    // ⚠ Refused BY NAME rather than swallowed by the unknown-argument arm. Both describe the
    // BACKEND's filesystem — the store root it reads and the pinned trainer it execs — so an
    // operator typing either believes they are steering a machine they cannot see.
    // `crate::cmd::backtest` refuses `--store` on its remote path for exactly this reason, and
    // saying WHY is the difference between a fixable message and a typo report.
    #[arg(long, hide = true, num_args = 0..=1)]
    store: Option<Option<String>>,
    #[arg(long, hide = true, num_args = 0..=1)]
    lightgbm: Option<Option<String>>,
}

/// The parsed `study` command line. PURE — the whole grammar is unit-tested below.
#[derive(Debug, PartialEq, Eq)]
struct Args {
    /// `--study`: the compiled study's registry name. Not validated here — the study registry is
    /// a compile-time const in `vike-backtest`/`vike-studio-core`, which this crate deliberately
    /// does not link (its identity is the light DataFusion-free graph), so the backend is the one
    /// side that can say whether a name exists.
    study: String,
    /// `--recipe`: the LOCAL path, kept for the error messages. Its TEXT is what travels.
    recipe_path: String,
    /// `--from`: the window's start, the operator's own spelling, parsed by the backend.
    from: String,
    /// `--to`: the window's end, likewise.
    to: String,
    /// `--addr`: the TOP rung of the address ladder, and `None` when it was not given — never a
    /// default filled in here. A parser that substituted the default would make the other two
    /// rungs unreachable, which is exactly how this verb came to carry a literal of its own.
    /// [`resolve_addr`] folds all three.
    addr: Option<String>,
    /// `--json`: print the server's own run document verbatim instead of the rendered lines.
    ///
    /// ⚠ It was REFUSED BY NAME until stage 7, with "it arrives with the request variant that
    /// produces an answer" — and this IS that variant. A refusal that outlived its reason is the
    /// `Policy::max_total_exposure` shape in the other direction.
    json: bool,
}

impl StudyCmd {
    /// The verb's own rules, after the parse: the two retired flags answered by name.
    ///
    /// ⚠ `--json` is ACCEPTED since stage 7, and its own refusal said why it would be: "the flag
    /// arrives with the request variant that returns an answer". `Request::RunStudy` IS that
    /// variant, so the refusal outlived its reason. The two flags above stay refused, for the
    /// reason that has NOT changed: they name paths on the BACKEND's box.
    fn into_args(self) -> Result<Args, String> {
        for (flag, given) in [("--store", &self.store), ("--lightgbm", &self.lightgbm)] {
            if given.is_some() {
                return Err(format!(
                    "{flag} names a path on the BACKEND's box, which this side cannot see — it is \
                     that daemon's configuration, not a flag of this command"
                ));
            }
        }
        Ok(Args {
            study: self.study,
            recipe_path: self.recipe,
            from: self.from,
            to: self.to,
            addr: self.addr,
            json: self.json,
        })
    }
}

/// Read a `study` argv tail — the arguments after `research study` — through the plane's own
/// parser, so a test meets the grammar an operator does.
#[cfg(test)]
fn parse(argv: impl Iterator<Item = String>) -> Result<Args, args::Stop> {
    let argv = std::iter::once("study".to_string()).chain(argv);
    match crate::cmd::research::parse(argv)? {
        crate::cmd::research::Sub::Study(cmd) => {
            cmd.into_args().map_err(|msg| args::Stop::message(BIN, msg))
        }
    }
}

/// The `vike-backend study` invocation that runs this study ON the backend's own box, carrying
/// every argument this invocation supplied.
/// ⚠ It is part of every failure this verb can produce rather than an aside, and that is a change:
/// it used to ride the capability refusal alone, so a failed dial — the ordinary outcome on a box
/// that has not stood the backtesthub up — printed a transport report and no way forward. Until
/// the server arm exists this line is the only way to run the study at all, and the two arguments
/// an operator most often gets wrong when they re-type it are the window bounds, so they are
/// carried verbatim.
/// most often gets wrong when they re-type it are the window bounds, so they are carried verbatim.
/// `--store` and `--lightgbm` are left as placeholders deliberately: they are facts about that
/// box, and a guess here would be this side inventing a path on a machine it cannot see.
fn backend_fallback(args: &Args) -> String {
    format!(
        "  vike-backend study --study {} --recipe {} --from {} --to {} \\\n\
         \x20   --store <the backend's hist store> [--lightgbm <the pinned trainer>]",
        args.study, args.recipe_path, args.from, args.to
    )
}

/// The lines for a peer that ANSWERED and does NOT advertise [`FEATURE_STUDY`].
///
/// ⚠ **It was "every peer that speaks this protocol today" until stage 7, and it is not any more.**
/// Every backtesthub advertises it (it calls the runner directly since the daemon moved to
/// `vike-backtesthub`); what still lands here is the DATA server (which never will) and a genuinely
/// older backend. The sentence was written to survive exactly this change — it names the CAPABILITY rather than asserting nobody serves it.
///
/// It states that nothing was sent, and hands over the invocation that works.
fn no_capability_lines(args: &Args, addr: &str, recipe_bytes: usize) -> Vec<String> {
    vec![
        format!(
            "vike-cli research study: the backend at {addr} does not advertise the {FEATURE_STUDY:?} \
             capability — the study was refused client-side, nothing was sent (the recipe read \
             fine: {recipe_bytes} bytes)"
        ),
        "that peer serves no study — every backtesthub (`vike-backend backtesthub --addr`, the \
         `backtesthub` bin) does, an older backend does not, and the data server never will. Or run \
         the study ON the box that holds the store:"
            .to_string(),
        backend_fallback(args),
    ]
}

/// The lines a FAILED DIAL writes — [`connect_failure_exit`]'s twin, split out so the words and
/// the number are decided over one `ErrorKind` match and cannot come to disagree.
///
/// The kinds are `crates/vike-datahub-client/src/client.rs`'s `DatahubClient::connect_authed` and
/// its key-less twin, and the split is the one that matters for the rung below:
/// [`io::ErrorKind::PermissionDenied`] (an `AuthDenied` frame, or a keyed server reached with no
/// keys) and [`io::ErrorKind::InvalidData`] (a protocol-version mismatch, a `Welcome` that carried
/// no nonce) are the BACKEND ANSWERING. Only a socket that never got an answer is a transport
/// fault.
///
/// ⚠ **Every arm ends with [`backend_fallback`], and the transport arm is why.** A box that has
/// not stood `vike-backend backtesthub --addr` up reaches nothing at all, so "cannot connect" is a
/// sentence plenty of operators will see from this verb, and printing it without the invocation
/// that works would leave that path the only one with no way forward. ⚠ This doc said the
/// daemon "has not landed" and called a failed dial THE ordinary outcome; ruling 7's daemon half
/// has since landed and ships as `deploy/vike-backtesthub.service`, so on a CONFIGURED box the dial
/// now succeeds and the capability refusal is what an operator meets. Both arms still carry the
/// fallback — which of them is commoner is a property of the box, not of the build.
fn connect_failure_lines(args: &Args, addr: &str, err: &io::Error) -> Vec<String> {
    let mut lines = match err.kind() {
        io::ErrorKind::PermissionDenied => vec![
            format!(
                "vike-cli research study: the backend at {addr} refused this connection: {err}"
            ),
            "the datahub hub keys this side presented did not verify — `vike-cli secrets path` \
             names the store they were read from"
                .to_string(),
        ],
        io::ErrorKind::InvalidData => {
            vec![format!("vike-cli research study: {addr} answered, but not this protocol: {err}")]
        }
        _ => vec![
            format!("vike-cli research study: cannot connect to the backend at {addr}: {err}"),
            "no backtesthub is listening there — `vike-backend backtesthub --addr` serves this \
             address, and it does not advertise the \"study\" capability yet in any case."
                .to_string(),
        ],
    };
    lines.push("run the study ON the box that holds the store instead:".to_string());
    lines.push(backend_fallback(args));
    lines
}

/// The RUNG a failed dial exits on.
///
/// ⚠ This used to be a blanket [`Exit::Connect`] over every dial failure, which put a backend that
/// ANSWERED — an auth refusal, a version mismatch — on the one rung
/// `crates/vike-cli/src/exit.rs`'s `Exit::Connect` promises *"a wrapper should back off and retry
/// on"*. Neither of those can succeed on a re-dial, so a wrapper would loop forever against a
/// reachable box. The rule is `crate::cmd::report`'s `failure_exit`, unchanged and now spelled the
/// same way here: **the backend ANSWERED ⇒ [`Exit::Failed`]**, and only a socket that never got an
/// answer is [`Exit::Connect`].
fn connect_failure_exit(err: &io::Error) -> Exit {
    match err.kind() {
        io::ErrorKind::PermissionDenied | io::ErrorKind::InvalidData => Exit::Failed,
        _ => Exit::Connect,
    }
}

/// Entry point the dispatcher routes to. `args` is everything AFTER the `study` subcommand;
/// `configured_addr` is the `config.backtest_addr` setting as the dispatcher resolved it
/// (the middle rung of [`resolve_addr`]'s ladder — a `src/cmd/` file reads no settings of its
/// own); `keys` is the DATAHUB node pair the dispatcher resolved (from the hub-key store), `None`
/// on a box that has none — which a key-less backend serves anyway.
pub(crate) fn run(
    cmd: StudyCmd,
    configured_addr: Option<&str>,
    keys: Option<&HubKeys>,
) -> ExitCode {
    let parsed = match cmd.into_args() {
        Ok(a) => a,
        Err(msg) => return args::refuse(BIN, msg),
    };
    let addr = resolve_addr(parsed.addr.as_deref(), configured_addr);

    // The recipe is read BEFORE the socket: a path typo is the commonest failure of this command
    // and it should not cost a round trip. The text is what would travel — see the module doc for
    // why a path may not.
    let recipe = match std::fs::read_to_string(Path::new(&parsed.recipe_path)) {
        Ok(text) => text,
        Err(e) => {
            eprintln!("vike-cli research study: cannot read recipe {}: {e}", parsed.recipe_path);
            return Exit::Failed.into();
        }
    };

    // ⚠ `Scope::Write`, not `Observe`: a study RUNS strategy code on the backend and WRITES a run
    // directory there, which is the class `vike_datahub::server`'s `required_scope` puts under
    // Control for every existing `Run*` verb. Negotiating under the weaker scope now would mean
    // the connection had to be re-opened the day the request exists. `None` keeps the
    // unauthenticated dial a key-less server has always answered.
    let client = match DatahubClient::connect_with_keys(&addr, keys, Scope::Write) {
        Ok(c) => c,
        Err(e) => {
            for line in connect_failure_lines(&parsed, &addr, &e) {
                eprintln!("{line}");
            }
            return connect_failure_exit(&e).into();
        }
    };

    // The negotiation. ⚠ A peer that answers and advertises nothing is the ORDINARY outcome against
    // the data daemon and against an older compute build — `vike_datahub_client::FEATURE_STUDY`
    // carries why.
    if !client.features().iter().any(|f| f == FEATURE_STUDY) {
        for line in no_capability_lines(&parsed, &addr, recipe.len()) {
            eprintln!("{line}");
        }
        // The backend ANSWERED — it completed a handshake and told us what it serves. That is a
        // permanent configuration fact about a reachable box, so it takes the pre-existing rung and
        // not the retry one, the same rule `crate::cmd::report`'s `failure_exit` applies.
        return Exit::Failed.into();
    }

    let mut client = client;
    let request = WireStudy {
        study: parsed.study.clone(),
        recipe_toml: recipe,
        from: parsed.from.clone(),
        to: parsed.to.clone(),
    };
    match client.run_study(&request) {
        Ok(json) => {
            if parsed.json {
                // Verbatim: exactly the document the server emitted, the rule
                // `crate::cmd::backtest`'s `--json` follows.
                println!("{json}");
                return Exit::Ok.into();
            }
            print_study(&json);
            Exit::Ok.into()
        }
        Err(e) => {
            eprintln!("vike-cli research study: {e}");
            Exit::Failed.into()
        }
    }
}

/// Render the backend's run document — the SAME two shapes `vike_studio_core::study_cli`'s `run`
/// prints locally, so the two routes are a rehearsal for each other.
///
/// Read by STRING out of a `serde_json::Value`, exactly as `crate::cmd::backtest`'s `print_paramscan`
/// does and for the same reason: the run-manifest and study-outcome types derive no `Deserialize`,
/// which is also why `Response::StudyReport` is text.
fn print_study(json: &str) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        // The server answered something that is not the run document. Printing it raw is strictly
        // better than swallowing it: the operator is the one who can tell what it is.
        println!("{json}");
        return;
    };
    let id = v.get("run_id").and_then(serde_json::Value::as_str).unwrap_or("<unknown>");
    let dir = v.get("dir").and_then(serde_json::Value::as_str).unwrap_or("<unknown>");
    println!("run {id} -> {dir}");
    for m in
        v.get("metrics").and_then(serde_json::Value::as_array).map(Vec::as_slice).unwrap_or(&[])
    {
        // ⚠ `vike_studio_core::study_run`'s `metric_json` shape: a `{name, value}` object, because
        // `serde_json` renders NaN and ±inf as `null` and a study's whole output is metrics.
        let name = m.get("name").and_then(serde_json::Value::as_str).unwrap_or("?");
        match m.get("value") {
            Some(serde_json::Value::Null) | None => println!("{name} = (non-finite)"),
            Some(value) => println!("{name} = {value}"),
        }
    }
}

#[path = "study_tests.rs"]
#[cfg(test)]
mod study_tests;
