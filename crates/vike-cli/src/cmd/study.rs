//! `vike-cli study` — ask the backend to run a compiled study over the hist store IT holds.
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
//! **`vike-backend backtest --addr <bind>`**. So this verb dials the compute daemon, and its
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
//! # ⚠ THE SCOPE DECISION, and why this file sends no request
//!
//! Serving a study from what already exists was measured and is not available:
//! `vike_datahub_client::proto`'s `Request` has `RunBacktest`/`RunSlice`/`RunSweep`/
//! `RunWalkforward`, every one of them "run this strategy over this window and rank the result".
//! A study is a different operation — a named entry in the compiled study registry, a recipe with
//! a `[learner]` table, a fitted model per fold and a minted run directory — and none of those
//! verbs carries any of it. So the verb needs a NEW wire request.
//!
//! **That request is deliberately not added here.** Ruling 7's split was in flight in a sibling
//! change when this file was written — the one lifting exactly those verbs out of `vike-datahub`'s
//! served surface — and a `RunStudy` variant added to the shared `Request` enum then would have
//! landed in the middle of that lift, the two changes on top of each other. ⚠ That lift HAS since
//! landed (`crates/vike-backtest/src/compute_server.rs` is the daemon, `deploy/vike-backtest.service`
//! ships it), so the reason to defer is spent and only the SHAPE survives: what is here is
//! everything on THIS side of the wire — the grammar, the local recipe read, the address ladder,
//! the keys, the handshake and the capability negotiation — turning on
//! `vike_datahub_client::proto`'s `FEATURE_STUDY`, which is where the request variant and the
//! server arm attach. That constant carries the same argument from the protocol's side, and names
//! the compute daemon as the side that will advertise it. The compute daemon does not advertise it
//! today, which is the whole of what is left to do.
//!
//! # ⚠ What an invocation actually does TODAY — two outcomes, not one
//!
//! Nothing sends a request, but the two ways of getting there are different and this section used
//! to name only the second, which made every copy of it false on the box an operator is standing
//! at. ⚠ **Which of the two is the ORDINARY one INVERTED when ruling 7's daemon landed**, and this
//! block said the opposite until then — it was written against a tree where no process bound the
//! compute address at all:
//!
//! 1. **The peer answers and does not advertise the capability** — [`no_capability_lines`], which
//!    names the missing capability, says nothing was sent, and carries the `vike-backend study`
//!    invocation with this run's own arguments filled in. This is now the ORDINARY outcome on a
//!    configured box: `vike-backend backtest --addr` ships (`deploy/vike-backtest.service`), it
//!    binds the compute address, and it serves ruling 7's seven compute verbs — `study` is simply
//!    not among them yet, so the handshake succeeds and the negotiation refuses. It is also
//!    reachable against any peer speaking this protocol, which is how it is TESTED:
//!    `crates/vike-cli/tests/study_report_refusal_cli.rs` drives the shipped binary against a REAL
//!    `vike_datahub::serve` and asserts both the words and that the server counted no frame after
//!    the handshake.
//! 2. **The dial fails** — [`connect_failure_lines`], which names the address, says which half
//!    failed, and carries the same invocation anyway, because an operator who cannot reach a
//!    compute daemon needs the command that works even more than one who reached a peer. Still the
//!    ordinary outcome on a box that has NOT stood the daemon up, which is every laptop and every
//!    fresh checkout — so both arms carry the escape hatch and neither is treated as the exception.
//!
//! Either way the operator ends with the SSH escape hatch spelled correctly, which is what this
//! verb owes them until the server arm exists.
//!
//! The opposite skew is handled too and is not hypothetical for long: a backend that DOES
//! advertise the capability, reached by a `vike-cli` built before the request variant existed,
//! gets [`client_too_old_lines`] — upgrade the client. A negotiated capability has two sides and
//! only saying one of them is how "it just does nothing" happens.
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
//! The grammar ([`parse`]), the address ladder ([`resolve_addr`]), the two negotiation messages
//! ([`no_capability_lines`], [`client_too_old_lines`]), the backend fallback
//! ([`backend_fallback`]) and both halves of the dial-failure mapping ([`connect_failure_lines`]
//! for the words, [`connect_failure_exit`] for the rung) are PURE and unit-tested below. The
//! NEGOTIATION itself is not pure and is not left to a unit test either:
//! `crates/vike-cli/tests/study_report_refusal_cli.rs` runs the shipped binary against a real
//! server of this protocol on a loopback port. What has no test is a SUCCESSFUL study — there is
//! no server arm to succeed against, which is precisely the state the follow-up removes.

use std::io;
use std::path::Path;
use std::process::ExitCode;

use vike_datahub_client::{DatahubClient, FEATURE_STUDY, NodeKeys, Scope};

use crate::cmd::args::{Flags, exit_for_parse_error, help_requested};
use crate::exit::Exit;

pub(crate) const USAGE: &str = "\
usage: vike-cli study --study NAME --recipe FILE --from WHEN --to WHEN [--addr host:port]

Ask the backend to run one COMPILED study over the hist store IT holds, and to leave a run
behind there. The store, the CPU and the pinned LightGBM trainer are the backend's; only the
recipe text and the window cross the wire.

⚠ NO DAEMON SERVES THIS YET, and on a box today none is even LISTENING: the compute daemon this
verb dials (`vike-backend backtest --addr`) is a follow-up, as is this verb's server arm. So a run
ends either at the dial or at the capability negotiation, and BOTH name `vike-backend study` — the
same work, on the box that holds the store, with these flags already filled in. Nothing goes on
the wire in either case.

  --study NAME     the study's folder name under user_data/research/studies/rust/
  --recipe FILE    its .toml recipe, on THIS machine — its TEXT is shipped, never its path
  --from/--to WHEN YYYY-MM-DD, YYYY-MM-DDTHH, or bare unix SECONDS (the backend parses them)
  --addr HOST:PORT the COMPUTE daemon (`vike-backend backtest --addr`, where ruling 7 puts every
                   verb that runs an engine). Resolved as --addr, then settings/config.toml's
                   `backtest_addr`, then 127.0.0.1:7880 — NOT 7879, which is the live
                   order-signing daemon, and not 7878, which is the data server
  -h, --help       this message

There is no --json: this command has no answer to shape yet, and a flag promising an output
format nothing can produce is worse than an absent one. It arrives with the request variant.

The store root and the LightGBM binary are NOT flags here: both name paths on the backend's
box, which this side cannot see. They are that daemon's configuration.";

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
}

/// Parse everything after the `study` subcommand. Accepts `--flag value` and `--flag=value` via
/// the shared [`crate::cmd::args`] glue; `-h`/`--help` short-circuits through [`help_requested`].
fn parse(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut study: Option<String> = None;
    let mut recipe_path: Option<String> = None;
    let mut from: Option<String> = None;
    let mut to: Option<String> = None;
    let mut addr: Option<String> = None;

    let mut flags = Flags::new(args);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--study" => study = Some(flags.value(&flag, inline)?),
            "--recipe" => recipe_path = Some(flags.value(&flag, inline)?),
            "--from" => from = Some(flags.value(&flag, inline)?),
            "--to" => to = Some(flags.value(&flag, inline)?),
            "--addr" => addr = Some(flags.value(&flag, inline)?),
            // ⚠ Refused BY NAME rather than swallowed by the unknown-argument arm. Both describe
            // the BACKEND's filesystem — the store root it reads and the pinned trainer it
            // execs — so an operator typing either believes they are steering a machine they
            // cannot see. `crate::cmd::backtest` refuses `--store` on its remote path for exactly
            // this reason, and saying WHY is the difference between a fixable message and a typo
            // report.
            "--store" | "--lightgbm" => {
                return Err(format!(
                    "{flag} names a path on the BACKEND's box, which this side cannot see — it is \
                     that daemon's configuration, not a flag of this command"
                ));
            }
            // ⚠ Refused by name too, and for the OPPOSITE reason to the two above: `--json` names
            // an output shape rather than a path, and every sibling remote verb takes one — so an
            // operator will type it here. This command has no answer to shape (the server arm is a
            // follow-up), and a flag that parsed, set a field and then changed nothing would be
            // the shape `Policy::max_total_exposure` was DELETED for: positive confirmation of
            // something false. It lands with the request variant that produces an answer.
            "--json" => {
                return Err(
                    "--json has nothing to format yet: no backend serves this verb, so this \
                     command's only output is a refusal naming `vike-backend study`. The flag \
                     arrives with the request variant that returns an answer"
                        .to_string(),
                );
            }
            "-h" | "--help" => return help_requested(),
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    let study = required(study, "--study NAME")?;
    let recipe_path = required(recipe_path, "--recipe FILE")?;
    let from = required(from, "--from WHEN")?;
    let to = required(to, "--to WHEN")?;
    Ok(Args { study, recipe_path, from, to, addr })
}

/// The COMPUTE daemon's address, folded from the three rungs ruling 7 names: `--addr <v>` →
/// `settings/config.toml`'s `backtest_addr` → [`vike_config::DEFAULT_BACKTEST_ADDR`].
///
/// ⚠ **This function exists because a literal used to sit where the last rung is.** The verb
/// carried its own `const DEFAULT_ADDR`, which had two costs and the second is the one that
/// matters: an operator's `backtest_addr` was read by nothing here, so a box configured for its
/// compute daemon still dialled the compiled-in address — and a compiled-in address is exactly
/// what goes stale when a port is reassigned. The number now has ONE spelling in the workspace,
/// in `vike-config`, shared with the daemon that binds it.
///
/// A BLANK `--addr`/`backtest_addr` is treated as absent rather than honoured: it can only have
/// come from an unset shell variable or an empty TOML string, neither of which is an address, and
/// dialing `""` would report a connect failure naming nothing. (The file layer already refuses a
/// value with no `:`; the flag has no such check, so this is the one place a `--addr ''` is
/// answered.)
fn resolve_addr(cli: Option<&str>, configured: Option<&str>) -> String {
    for rung in [cli, configured] {
        if let Some(v) = rung
            && !v.trim().is_empty()
        {
            return v.trim().to_string();
        }
    }
    vike_config::DEFAULT_BACKTEST_ADDR.to_string()
}

/// One required flag, refused by name AND rejected when it was given an empty value.
///
/// The empty check is not decoration: `--from ''` reaches the backend as a string it cannot parse,
/// so the round trip is spent to learn something visible here — and `--study ''` would ask the
/// registry for a study whose name is nothing.
fn required(value: Option<String>, spelling: &str) -> Result<String, String> {
    match value {
        Some(v) if !v.trim().is_empty() => Ok(v),
        Some(_) => Err(format!("{spelling} was given an empty value")),
        None => Err(format!("{spelling} is required")),
    }
}

/// The `vike-backend study` invocation that runs this study ON the backend's own box, carrying
/// every argument this invocation supplied.
/// ⚠ It is part of every failure this verb can produce rather than an aside, and that is a change:
/// it used to ride the capability refusal alone, so a failed dial — the ordinary outcome on a box
/// that has not stood the compute daemon up — printed a transport report and no way forward. Until
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

/// The lines for a peer that ANSWERED and does NOT advertise [`FEATURE_STUDY`] — every peer that
/// speaks this protocol today.
///
/// It names the capability (so the sentence stays true when the follow-up lands and this becomes
/// the genuinely-old-server message), states that nothing was sent, and hands over the invocation
/// that works.
fn no_capability_lines(args: &Args, addr: &str, recipe_bytes: usize) -> Vec<String> {
    vec![
        format!(
            "vike-cli study: the backend at {addr} does not advertise the {FEATURE_STUDY:?} \
             capability — the study was refused client-side, nothing was sent (the recipe read \
             fine: {recipe_bytes} bytes)"
        ),
        "no backend serves this verb yet — the server half is a follow-up of ruling 16. Until it \
         lands, run the study ON the box that holds the store:"
            .to_string(),
        backend_fallback(args),
    ]
}

/// The OPPOSITE skew: a backend that DOES advertise the capability, reached by a `vike-cli` built
/// before the request variant existed.
///
/// ⚠ This arm is unreachable today and is written anyway, because a negotiation with only one side
/// implemented is how a capability silently does nothing. The day the server arm ships, every
/// client older than it lands here and is told the one thing that fixes it.
fn client_too_old_lines(addr: &str) -> Vec<String> {
    vec![
        format!(
            "vike-cli study: the backend at {addr} serves {FEATURE_STUDY:?}, but this vike-cli \
             build has no request for it — nothing was sent"
        ),
        "upgrade vike-cli to a build carrying the study request, then re-run".to_string(),
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
/// not stood `vike-backend backtest --addr` up reaches nothing at all, so "cannot connect" is a
/// sentence plenty of operators will see from this verb, and printing it without the invocation
/// that works would leave that path the only one with no way forward. ⚠ This doc said the
/// daemon "has not landed" and called a failed dial THE ordinary outcome; ruling 7's daemon half
/// has since landed and ships as `deploy/vike-backtest.service`, so on a CONFIGURED box the dial
/// now succeeds and the capability refusal is what an operator meets. Both arms still carry the
/// fallback — which of them is commoner is a property of the box, not of the build.
fn connect_failure_lines(args: &Args, addr: &str, err: &io::Error) -> Vec<String> {
    let mut lines = match err.kind() {
        io::ErrorKind::PermissionDenied => vec![
            format!("vike-cli study: the backend at {addr} refused this connection: {err}"),
            "the datahub node keys this side presented did not verify — `vike-cli secrets path` \
             names the store they were read from"
                .to_string(),
        ],
        io::ErrorKind::InvalidData => {
            vec![format!("vike-cli study: {addr} answered, but not this protocol: {err}")]
        }
        _ => vec![
            format!("vike-cli study: cannot connect to the backend at {addr}: {err}"),
            "no compute daemon is listening there — `vike-backend backtest --addr` serves this \
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
/// `configured_addr` is `settings/config.toml`'s `backtest_addr` as the dispatcher resolved it
/// (the middle rung of [`resolve_addr`]'s ladder — a `src/cmd/` file reads no settings of its
/// own); `keys` is the DATAHUB node pair the dispatcher resolved (process environment first, then
/// the node-key store), `None` on a box that has neither — which a key-less backend serves anyway.
pub fn run(
    args: impl Iterator<Item = String>,
    configured_addr: Option<&str>,
    keys: Option<&NodeKeys>,
) -> ExitCode {
    let parsed = match parse(args) {
        Ok(a) => a,
        Err(msg) => return exit_for_parse_error("study", USAGE, &msg),
    };
    let addr = resolve_addr(parsed.addr.as_deref(), configured_addr);

    // The recipe is read BEFORE the socket: a path typo is the commonest failure of this command
    // and it should not cost a round trip. The text is what would travel — see the module doc for
    // why a path may not.
    let recipe = match std::fs::read_to_string(Path::new(&parsed.recipe_path)) {
        Ok(text) => text,
        Err(e) => {
            eprintln!("vike-cli study: cannot read recipe {}: {e}", parsed.recipe_path);
            return Exit::Failed.into();
        }
    };

    // ⚠ `Scope::Control`, not `Observe`: a study RUNS strategy code on the backend and WRITES a run
    // directory there, which is the class `vike_datahub::server`'s `required_scope` puts under
    // Control for every existing `Run*` verb. Negotiating under the weaker scope now would mean
    // the connection had to be re-opened the day the request exists. `None` keeps the
    // unauthenticated dial a key-less server has always answered.
    let client = match keys {
        Some(k) => DatahubClient::connect_authed(&addr, k, Scope::Control),
        None => DatahubClient::connect(&addr),
    };
    let client = match client {
        Ok(c) => c,
        Err(e) => {
            for line in connect_failure_lines(&parsed, &addr, &e) {
                eprintln!("{line}");
            }
            return connect_failure_exit(&e).into();
        }
    };

    // The negotiation, and both of its sides. Nothing goes on the wire after the handshake on
    // either arm — see the module doc for why that is the whole of this verb today.
    let served = client.features().iter().any(|f| f == FEATURE_STUDY);
    let lines = if served {
        client_too_old_lines(&addr)
    } else {
        no_capability_lines(&parsed, &addr, recipe.len())
    };
    for line in lines {
        eprintln!("{line}");
    }
    // The backend ANSWERED — it completed a handshake and told us what it serves. That is a
    // permanent configuration fact about a reachable box, so it takes the pre-existing rung and
    // not the retry one, the same rule `crate::cmd::report`'s `failure_exit` applies.
    Exit::Failed.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::args::HELP_SENTINEL;

    fn parsed(args: &[&str]) -> Result<Args, String> {
        parse(args.iter().map(|s| s.to_string()))
    }

    fn full() -> Args {
        Args {
            study: "cohort".to_string(),
            recipe_path: "r.toml".to_string(),
            from: "2026-04-07T05".to_string(),
            to: "2026-08-05T05".to_string(),
            addr: None,
        }
    }

    // ---- the grammar ----

    #[test]
    fn the_four_required_flags_parse_and_the_address_is_left_unset() {
        let a = parsed(&[
            "--study",
            "cohort",
            "--recipe",
            "r.toml",
            "--from",
            "2026-04-07T05",
            "--to",
            "2026-08-05T05",
        ])
        .unwrap();
        assert_eq!(a, full());
    }

    #[test]
    fn the_inline_form_parses_and_the_address_is_overridable() {
        let a = parsed(&[
            "--study=cohort",
            "--recipe=r.toml",
            "--from=1785906000",
            "--to=1788584400",
            "--addr=<host>:9999",
        ])
        .unwrap();
        assert_eq!(a.addr.as_deref(), Some("<host>:9999"));
        assert_eq!(a.from, "1785906000");
    }

    // ---- the address ladder ----

    /// The three rungs, in order, over one table — `--addr` beats the settings key, the settings
    /// key beats the compiled-in default, and the default is `vike-config`'s rather than a literal
    /// of this module's.
    #[test]
    fn the_address_ladder_is_flag_then_setting_then_the_shared_default() {
        assert_eq!(resolve_addr(Some("<host>:1"), Some("<host>:2")), "<host>:1");
        assert_eq!(resolve_addr(None, Some("<host>:2")), "<host>:2");
        assert_eq!(resolve_addr(None, None), vike_config::DEFAULT_BACKTEST_ADDR);
    }

    /// A blank rung is ABSENT, not an address: an unset shell variable and an empty TOML string
    /// both arrive here as `""`, and dialing that would report a failure naming nothing.
    #[test]
    fn a_blank_rung_falls_through_rather_than_being_dialled() {
        assert_eq!(resolve_addr(Some("   "), Some("<host>:2")), "<host>:2");
        assert_eq!(resolve_addr(Some(""), None), vike_config::DEFAULT_BACKTEST_ADDR);
        assert_eq!(resolve_addr(None, Some(" ")), vike_config::DEFAULT_BACKTEST_ADDR);
        // …and a value that IS an address is trimmed rather than dialled with its whitespace.
        assert_eq!(resolve_addr(Some(" <host>:1 "), None), "<host>:1");
    }

    /// ⚠ The default is the COMPUTE daemon's, and this test is a THREE-WAY separation because two
    /// of the three numbers are other people's daemons: `127.0.0.1:7879` is the LIVE ORDER-SIGNING
    /// daemon (`ss -ltnp` on the production box, 2026-09-10) and `127.0.0.1:7878` is the data
    /// server ruling 7 empties. A default equal to either would aim this verb at a process that
    /// does not serve it — one of them the one that signs orders.
    #[test]
    fn the_default_address_is_the_compute_daemons_and_neither_neighbours() {
        let dialled = resolve_addr(None, None);
        assert_eq!(dialled, "127.0.0.1:7880");
        assert_ne!(dialled, "127.0.0.1:7879", "that is the LIVE order-signing daemon");
        assert_ne!(dialled, "127.0.0.1:7878", "that is the DATA server ruling 7 empties");
    }

    /// Every required flag is refused BY NAME when absent — one test over all four, because a
    /// message that names the wrong flag is the defect worth catching.
    #[test]
    fn each_missing_required_flag_is_named() {
        let base = ["--study=c", "--recipe=r.toml", "--from=a", "--to=b"];
        for (i, missing) in ["--study", "--recipe", "--from", "--to"].iter().enumerate() {
            let args: Vec<&str> =
                base.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, s)| *s).collect();
            let err = parsed(&args).unwrap_err();
            assert!(err.contains(missing), "expected {missing} to be named: {err}");
        }
    }

    /// An EMPTY value is refused too — it would otherwise reach the backend as a string it cannot
    /// parse, spending a round trip on something visible here.
    #[test]
    fn an_empty_required_value_is_refused_here_not_on_the_backend() {
        let err = parsed(&["--study=", "--recipe=r.toml", "--from=a", "--to=b"]).unwrap_err();
        assert!(err.contains("--study"), "{err}");
        assert!(err.contains("empty"), "{err}");
    }

    /// The two BACKEND-side paths are refused by name with the reason, never dropped into the
    /// generic unknown-argument arm: an operator passing one believes they are steering a machine
    /// this side cannot see.
    #[test]
    fn the_backend_side_paths_are_refused_with_a_reason() {
        for flag in ["--store", "--lightgbm"] {
            let err =
                parsed(&["--study=c", "--recipe=r.toml", "--from=a", "--to=b", flag, "/somewhere"])
                    .unwrap_err();
            assert!(err.contains(flag), "{err}");
            assert!(err.contains("BACKEND"), "it says whose box the path is on: {err}");
        }
    }

    /// `--json` is refused by NAME with the reason rather than accepted-and-ignored. Every sibling
    /// remote verb takes one, so an operator will type it here — and a flag that parsed and then
    /// changed nothing would be confirming an output shape this command cannot produce.
    #[test]
    fn json_is_refused_by_name_because_there_is_no_answer_to_shape_yet() {
        let args = ["--study=c", "--recipe=r.toml", "--from=a", "--to=b", "--json"];
        let err = parsed(&args).unwrap_err();
        assert!(err.contains("--json"), "{err}");
        assert!(err.contains("nothing to format yet"), "it says WHY: {err}");
    }

    #[test]
    fn help_short_circuits_even_without_the_required_flags() {
        for spelling in ["-h", "--help"] {
            assert_eq!(parsed(&[spelling]).unwrap_err(), HELP_SENTINEL);
        }
    }

    #[test]
    fn an_unknown_argument_is_rejected_by_name() {
        let err =
            parsed(&["--study=c", "--recipe=r.toml", "--from=a", "--to=b", "--scratch=/tmp/x"])
                .unwrap_err();
        assert!(err.contains("--scratch"), "{err}");
    }

    // ---- the negotiation's two sides ----

    /// The refusal a peer that ANSWERS produces: it names the capability, says nothing was sent,
    /// confirms the recipe was readable (so a reader knows which half failed), and carries the
    /// invocation that works today with this run's own arguments in it.
    ///
    /// ⚠ These are the WORDS only. That this path is REACHED at all — a real server of this
    /// protocol, a real handshake, and no frame after it — is
    /// `crates/vike-cli/tests/study_report_refusal_cli.rs`, because a unit test over a pure
    /// formatter cannot tell the difference between a message an invocation produces and one
    /// nothing can reach.
    #[test]
    fn a_backend_without_the_capability_is_told_what_works_today() {
        let lines = no_capability_lines(&full(), "127.0.0.1:7880", 412);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[0].contains(FEATURE_STUDY), "{}", lines[0]);
        assert!(lines[0].contains("nothing was sent"), "{}", lines[0]);
        assert!(lines[0].contains("412 bytes"), "the recipe read is reported: {}", lines[0]);
        assert!(lines[2].contains("vike-backend study"), "{}", lines[2]);
        assert!(lines[2].contains("--study cohort"), "{}", lines[2]);
        assert!(lines[2].contains("--from 2026-04-07T05"), "the window is carried: {}", lines[2]);
        assert!(lines[2].contains("--to 2026-08-05T05"), "the window is carried: {}", lines[2]);
    }

    /// …and the OPPOSITE skew has its own message. Unreachable today, written anyway: a
    /// negotiation whose other side says nothing is how a shipped capability silently does
    /// nothing.
    #[test]
    fn a_backend_ahead_of_this_client_is_told_to_upgrade_the_client() {
        let lines = client_too_old_lines("127.0.0.1:7880");
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].contains(FEATURE_STUDY), "{}", lines[0]);
        assert!(lines[0].contains("nothing was sent"), "{}", lines[0]);
        assert!(lines[1].contains("upgrade vike-cli"), "{}", lines[1]);
    }

    /// The fallback names no path this side cannot know: the store root and the trainer stay
    /// placeholders, because inventing either would be this machine describing another one's
    /// filesystem.
    #[test]
    fn the_fallback_leaves_the_backend_only_paths_as_placeholders() {
        let line = backend_fallback(&full());
        assert!(line.contains("<the backend's hist store>"), "{line}");
        assert!(line.contains("<the pinned trainer>"), "{line}");
        assert!(line.contains("--recipe r.toml"), "the LOCAL recipe path is real: {line}");
    }

    // ---- the dial failure: the words, and the rung ----

    /// The RUNG, pinned kind by kind against the rule `crate::cmd::report`'s `failure_exit`
    /// states: **the backend ANSWERED** ⇒ the pre-existing rung, and only a socket that never got
    /// an answer is the retry rung. The two ANSWERED rows are the load-bearing ones — an auth
    /// refusal and a protocol-version mismatch are permanent configuration facts about a REACHABLE
    /// box, and on `Exit::Connect` a wrapper would back off and re-dial forever.
    #[test]
    fn the_rung_follows_whether_the_backend_answered() {
        for kind in [io::ErrorKind::PermissionDenied, io::ErrorKind::InvalidData] {
            let err = io::Error::new(kind, "the backend said something");
            assert_eq!(connect_failure_exit(&err), Exit::Failed, "{kind:?}: it ANSWERED");
        }
        for kind in [
            io::ErrorKind::ConnectionRefused,
            io::ErrorKind::TimedOut,
            io::ErrorKind::ConnectionAborted,
        ] {
            let err = io::Error::new(kind, "no answer");
            assert_eq!(connect_failure_exit(&err), Exit::Connect, "{kind:?} never answered");
        }
    }

    /// …and the WORDS follow the same split: an auth refusal points at the keys, a protocol
    /// mismatch says the backend answered with something else, and everything else is an honest
    /// transport report naming the address. EVERY arm ends with the invocation that works — the
    /// transport one above all, because a box with nothing listening on the compute address is the
    /// ordinary box today.
    #[test]
    fn a_refused_dial_says_which_half_failed_and_still_names_what_works() {
        let addr = "127.0.0.1:7880";
        for (kind, needle) in [
            (io::ErrorKind::PermissionDenied, "did not verify"),
            (io::ErrorKind::InvalidData, "answered"),
            (io::ErrorKind::ConnectionRefused, "cannot connect"),
        ] {
            let err = io::Error::new(kind, "something");
            let lines = connect_failure_lines(&full(), addr, &err);
            let text = lines.join("\n");
            assert!(text.contains(addr), "{kind:?} must name the address: {text}");
            assert!(text.contains(needle), "{kind:?}: {text}");
            assert!(
                lines.last().is_some_and(|l| l.contains("vike-backend study")),
                "{kind:?} must still hand over the command that works: {text}"
            );
        }
    }

    /// The transport arm carries one extra sentence the other two do not, and it is the one an
    /// operator most needs when this fires: nothing is listening on the compute address, so a
    /// refused connection is a daemon that was never stood up rather than a broken tunnel — and
    /// standing it up would not make the verb WORK either, because `vike-backend backtest --addr`
    /// does not advertise `study` yet. ⚠ This doc said the daemon "has not shipped"; ruling 7's
    /// daemon half has since landed, so the sentence had to stop blaming its absence.
    #[test]
    fn a_dead_socket_says_why_nothing_is_listening_there_yet() {
        let err = io::Error::new(io::ErrorKind::ConnectionRefused, "connection refused");
        let text = connect_failure_lines(&full(), "127.0.0.1:7880", &err).join("\n");
        assert!(text.contains("no compute daemon is listening there"), "{text}");
        assert!(text.contains("backtest --addr"), "it names the daemon: {text}");
    }
}
