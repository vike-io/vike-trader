//! The WRITE path: preview + confirm + send, the one-shot `halt`/`resume`, and the settings write.

use std::process::ExitCode;
use std::time::Duration;

use rustyline::DefaultEditor;
use vike_tradehub_client::wire::{WireCommand, WireTradingState};
use vike_tradehub_client::{CommandOutcome, ControlRejected, RemoteControlHandle};

use crate::cmd::args;
use crate::cmd::hubkeys::{self, HubKeyring};
use crate::cmd::verbs;
use crate::exit::Exit;

use super::describe::describe_command;
use super::{ModeArgs, NoControl, Session};

/// Preview + confirm + send one WRITE command. The node's server-side gate is the real enforcer.
/// `reason` is the optional operator rationale: shown in the preview and sent BESIDE the command for
/// the node's audit trail — it never becomes part of the order.
///
/// It takes the resolved [`WireCommand`] rather than a [`Verb`] because everything below this
/// point is about the wire form: the coid mint, the preview line, the guardrail and the settings
/// branch all read the command's VARIANT. Both grammars resolve through the one
/// [`Verb::to_wire_command`] before they get here.
pub(super) fn run_write(
    session: &mut Session<'_>,
    rl: &mut DefaultEditor,
    cmd: WireCommand,
    reason: Option<String>,
) {
    // MINT the client-order-id before anything is printed. Two reasons, both load-bearing: the node
    // REFUSES a remote submit with an empty one (so an unminted command can never be placed), and
    // the preview must show the id that will actually be sent — it is what the operator reads off
    // the screen and types back at `cancel <coid>`. An explicit `--coid` is left alone.
    let cmd = verbs::fill_client_order_id(cmd, &mut session.coids);
    // PREVIEW — always, even in --yes mode, so the human sees exactly what will be sent. The
    // guardrail is the SHARED check in `verbs` (same semantics as the mcp tools' preview).
    println!("preview: {}", describe_command(&cmd));
    println!("  {}", verbs::guardrail_check(&cmd, session.caps).line());
    // A coid this REPL could not have minted and would have REFUSED on `submit`. Advisory: it is
    // still sent (see [`verbs::coid_charset_warning`] for why refusing would be wrong).
    if let Some(w) = verbs::coid_charset_warning(&cmd) {
        println!("  {w}");
    }
    if let Some(why) = &reason {
        println!("  reason (recorded in the node's audit trail): {why}");
    }
    // A SETTINGS write shows what it CHANGES before the y/N: `old → new`, the old value read off
    // the node this write is aimed at. That line, the node's own bounds check and its journal are
    // what guard a live limit since `docs/decisions/0086` point 7 took the retype away.
    let change = match &cmd {
        WireCommand::SetSetting { key, value, .. } => {
            Some(read_setting_change(session, key, value))
        }
        _ => None,
    };
    if let Some(change) = &change {
        println!("  change: {}", change.line());
        if let Some(env) = change.env_shadow() {
            println!(
                "  ⚠ that current value is set by {env} on the node, which outranks a settings row \
                 — this write lands the row, and the node keeps following {env} while it is set"
            );
        }
    }

    // The write gate, checked WITHOUT holding a borrow of the handle: the settings branch below
    // hands the whole session to [`run_set_setting`], which reaches the node through its own
    // connection and therefore needs `session` again.
    match session.control.as_ref() {
        None => {
            // NAME THE ACTUAL REASON. This used to be one hard-coded "no VIKE_TRADEHUB_CONTROL_KEY"
            // line for all three causes; with the node's control flag off and a valid key in the
            // store it was simply false, and the true reason had already scrolled past at connect
            // time.
            println!(
                "{}",
                session
                    .control_denied
                    .as_ref()
                    .map_or_else(|| NoControl::NoKey.refusal_line(), NoControl::refusal_line)
            );
            return;
        }
        Some(control) if !control.is_connected() => {
            println!("[error] control connection is down — nothing was sent");
            return;
        }
        Some(_) => {}
    }

    if !session.yes && !confirm(rl) {
        println!("aborted — nothing was sent");
        return;
    }

    // A SETTINGS write leaves the fire-and-forget path here. Not a special case for its own sake:
    // the shared worker maps an accepted `SetSetting` to an empty-coid `Accepted` and DROPS the
    // reply's `restart_required`, so this path would have to tell the operator "accepted" while
    // knowing nothing about whether the value is live — the one question a settings write is asked.
    // `change` is `Some` exactly for this variant: it was built from the same match above.
    if let (WireCommand::SetSetting { file, key, value, .. }, Some(change)) = (&cmd, &change) {
        run_set_setting(session, file, key, value, change, reason.as_deref());
        return;
    }

    let Some(control) = session.control.as_ref() else {
        // Unreachable: the gate above returned on `None`. Kept as a `let-else` rather than an
        // `expect` so a future edit that moves the gate degrades to a message instead of a panic in
        // an order surface.
        println!("[error] control connection is down — nothing was sent");
        return;
    };
    send_and_report(control, cmd, reason, session.ack_wait);
}

/// Send ONE resolved [`WireCommand`] on an open control connection and report the node's verdict on
/// THAT command. Returns whether the node ACCEPTED it, which the one-shot verbs turn into an exit
/// code and the REPL ignores (a REPL has no exit code, only the printed line).
///
/// ⚠ Extracted from [`run_write`] when `vike-cli trade halt` / `resume` became one-shot verbs
/// (ruling 17), and extracted rather than re-rolled for the reason the outcome arms below spell out
/// at length: the difference between `Disconnected` ("it may or may not have executed") and
/// `NeverSent` ("nothing went to the node") is the whole safety content of this surface, and a
/// second copy of that reasoning is a second copy to get wrong. It never polls
/// [`RemoteControlHandle::last_error`], which LATCHES — see this module's doc for what that cost.
fn send_and_report(
    control: &RemoteControlHandle,
    cmd: WireCommand,
    reason: Option<String>,
    ack_wait: Duration,
) -> bool {
    // The ticket is THIS command's identity; its outcome is reported for it and no other.
    match control.try_command_with_reason(cmd, reason) {
        Ok(ticket) => match control.await_outcome(ticket, ack_wait) {
            Some(CommandOutcome::Accepted { coid }) => {
                let named = if coid.is_empty() { String::new() } else { format!(" (coid {coid})") };
                println!(
                    "accepted by the node{named} — run `orders`/`positions` to observe the result; \
                     the node's ControlLimits + RiskGate are the enforcing gate"
                );
                return true;
            }
            Some(CommandOutcome::Refused(err)) => println!("node rejected the command: {err}"),
            // The connection died before the reply: genuinely UNKNOWN, so say so. Printing "sent"
            // here (what the old latch-poll did, since it saw no error) is the same class of lie as
            // the stale refusal, pointing the other way.
            Some(CommandOutcome::Disconnected) => println!(
                "[error] NOT CONFIRMED: the control connection dropped before the node answered \
                 this command — it may or may not have executed. Reconnect and check \
                 `orders`/`positions` before retrying."
            ),
            // NOT the same statement as the arm above, and the difference is what the operator
            // needs: the link was already closed when the sender reached this command, so nothing
            // was written and nothing can have executed. Safe to re-issue verbatim once the
            // session is reconnected — no book check required first. (This REPL holds one handle
            // for the session and does not reconnect by itself, deliberately: `trade` is a human
            // at a prompt who can see this line and re-run the command, unlike the MCP server,
            // whose caller is an agent mid-tool-call.)
            Some(CommandOutcome::NeverSent) => println!(
                "not sent: the control connection was already closed when this command reached \
                 the sender, so NOTHING went to the node and nothing executed. Reconnect \
                 (re-run `vike-cli trade`) and issue it again."
            ),
            None => println!(
                "sent, but the node has not answered within {} — the outcome is NOT yet known; \
                 check `orders`/`positions` before retrying",
                verbs::wait_seconds_text(ack_wait)
            ),
        },
        // A CAPABILITY refusal is the one rejection an operator can act on, and `{e:?}` renders it
        // as the bare token `UnsupportedByHub`. It is also the arm the lifecycle verbs make
        // reachable in ordinary use: `mount`/`unmount` need `mount-verbs`, which a node older than
        // split-plane B5 does not advertise, and the refusal happens CLIENT-side — nothing was
        // enqueued and nothing went on the wire.
        Err(ControlRejected::UnsupportedByHub) => println!(
            "not sent: this node does not advertise the capability this verb requires (an older \
             vike-tradehub) — the command was refused CLIENT-side, so nothing went on the wire. \
             Upgrade the node, or use a verb it speaks."
        ),
        Err(e) => println!("not sent: {e:?}"),
    }
    // Every arm above except `Accepted` (which returns early) is a NOT-accepted outcome — including
    // the two that are genuinely UNKNOWN. A caller turning this into an exit code must read `false`
    // as "do not assume it landed", never as "nothing happened"; the printed line is what says
    // which of the two it was.
    false
}

/// `vike-cli trade halt` / `vike-cli trade resume` — ONE mode write, then exit (ruling 17).
///
/// The same three words as the REPL's, doing the same thing, with the same preview + `confirm?
/// [y/N]` gate and the same `--yes` escape: a kill switch an operator reaches two ways must not
/// behave differently depending on which. `--reason <text to end of line>` rides along exactly as
/// it does at the prompt.
///
/// ⚠ **The confirm reads plain stdin here rather than the line editor**, because a one-shot is
/// routinely piped or run from a unit file where `rustyline` has no terminal to own. It is an
/// ordinary blocking line read, and saying more than that has already gone wrong once — see
/// [`confirm_stdin`], whose doc is the authority on exactly what a non-terminal stdin does. The
/// operative rule for a caller: **an unattended run must pass `--yes`**, because without it the
/// answer depends on what is on the other end of the pipe.
///
/// Exit rungs: `Exit::Usage` for a bad command line, `FAILURE` for no control key / a refused or
/// unconfirmed command, `Exit::Connect` when the control connection could not be opened at all, and
/// SUCCESS only when the node ACCEPTED the write. An aborted confirm is a FAILURE rather than a
/// success: nothing was sent, and a wrapper must not read "the operator said no" as "the node is
/// halted".
pub(super) fn run_set_mode_once(
    cmd: ModeCmd,
    keys: &HubKeyring,
    state: WireTradingState,
    ack_wait: Duration,
) -> ExitCode {
    let verb = mode_verb(state);
    let bin = format!("vike-cli trade {verb}");
    // `--reason` is a rest-of-line tail at the REPL and a flag here — the same value, taken the way
    // each surface's own grammar takes values. The REPL splits it BEFORE the verb grammar sees it
    // (`split_reason`); an argv reader has no line to split.
    let ModeArgs { node, yes, reason } = match cmd.into_args() {
        Ok(v) => v,
        Err(msg) => return args::refuse(&bin, msg),
    };
    let Some(node) = node else {
        return args::refuse(&bin, "--node <host:port> is required");
    };
    // The CONTROL key specifically: this is a write, and an observe key cannot authenticate one.
    let Some((key, origin)) = keys.control() else {
        eprintln!("vike-cli trade {verb}: {}", NoControl::NoKey.refusal_line());
        return ExitCode::FAILURE;
    };
    let control = match RemoteControlHandle::connect_with_reply_timeout(
        node.as_str(),
        key.as_bytes(),
        keys.patience().control_reply,
    ) {
        Ok(h) => h,
        Err(e) => {
            let cause = NoControl::from_connect_error(&e);
            eprintln!("vike-cli trade {verb}: {}", cause.refusal_line());
            // A REFUSED scope is a configuration fact about a reachable node (turn the node's flag
            // on), not a socket to retry — the `trade::status` ladder's rule, applied to the write
            // side.
            return match cause {
                NoControl::Refused(_) => ExitCode::FAILURE,
                _ => Exit::Connect.into(),
            };
        }
    };
    println!("connected: CONTROL (write) to {node} [key from the {}]", origin.label());

    let cmd = WireCommand::SetTradingState(state);
    println!("preview: {}", describe_command(&cmd));
    if let Some(why) = &reason {
        println!("  reason (recorded in the node's audit trail): {why}");
    }
    if !yes && !confirm_stdin() {
        println!("aborted — nothing was sent");
        return ExitCode::FAILURE;
    }
    if send_and_report(&control, cmd, reason, ack_wait) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// The word the operator typed, for every message this one-shot prints. Derived from the STATE so
/// the two can never disagree — a message naming `halt` while sending `Active` is the exact class
/// of confusion ruling 17 exists to remove.
pub(super) fn mode_verb(state: WireTradingState) -> &'static str {
    match state {
        WireTradingState::Halted => "halt",
        WireTradingState::Active => "resume",
        // Unreachable: no verb produces `Reducing` — it has no CLI spelling by ruling 17, and is
        // reached through `halt`'s covered-reduce behaviour (this module's doc). Kept total rather
        // than `unreachable!`, because a panic in an order surface is never the better answer.
        WireTradingState::Reducing => "reduce",
    }
}

/// `--help`'s prose for one mode verb (the `usage:` line and the options table are `clap`'s),
/// rendered from the verb's own name so the two cannot drift.
pub(super) fn mode_prose(verb: &str) -> String {
    let does = if verb == "halt" {
        "Set the node's trading mode to HALTED: the gate refuses every order that OPENS or ADDS \
         risk.\nPosition-covered reduces still pass, so `market-exit` and `flatten` keep working — \
         a halt\nstops trading, it does not trap you."
    } else {
        "Set the node's trading mode back to ACTIVE: ordinary trading resumes."
    };
    format!(
        "{does}\n\n\
         It needs a CONTROL-scope key (VIKE_TRADEHUB_OBSERVE_KEY cannot authenticate a write) and \
         a node\nwith `tradehub_control` on. `vike-cli trade status` is the READ — no spelling of \
         it writes.\n\n\
         ⚠ ALWAYS pass --yes when nothing is at the keyboard. Without it this reads one line from \
         stdin:\n  an EOF (a closed stdin, `< /dev/null`, a systemd unit) is a NO and nothing is \
         sent;\n  a `y` on a pipe CONFIRMS — a pipe is not a refusal;\n  \
         a stdin that never delivers a line BLOCKS, which in an incident is worse than either."
    )
}

// The command line of the one-shot mode verbs `halt` and `resume`, as `clap` reads it. `--node`
// is required, but that is judged after the parse ([`ModeCmd::into_args`]) so `--help` never has
// to supply one.
//
// `--reason` takes exactly ONE value here (unlike the write verbs' rest-of-argv `--reason`,
// `crate::cmd::trade::oneshot::WrapperArgs`): a value beginning with a single dash is a value
// (`--reason -urgent`), and one beginning with `--` is refused by [`ModeCmd::into_args`].
#[derive(clap::Args, Debug)]
pub(super) struct ModeCmd {
    #[arg(
        long,
        value_name = "HOST:PORT",
        overrides_with = "node",
        help = "the node's control address (required)"
    )]
    node: Option<String>,
    #[arg(
        short = 'y',
        long,
        overrides_with = "yes",
        help = "skip the confirm? [y/N] prompt (see the warning in --help)"
    )]
    yes: bool,
    #[arg(
        long,
        value_name = "TEXT",
        overrides_with = "reason",
        allow_hyphen_values = true,
        help = "operator rationale, recorded in the node's audit trail"
    )]
    reason: Option<String>,
}

impl ModeCmd {
    /// `--node` / `--yes` / `--reason` after the parse. PURE — unit-tested below.
    pub(super) fn into_args(self) -> Result<ModeArgs, String> {
        // `--reason --yes` used to be refused ("a valued flag may not eat a flag"); the option
        // accepts any single-dash value, so the `--` rule is kept here, in its old words.
        if let Some(found) = self.reason.as_deref()
            && found.starts_with("--")
        {
            return Err(format!(
                "--reason requires a value, but the next argument is another flag ({found})"
            ));
        }
        Ok(ModeArgs { node: self.node, yes: self.yes, reason: self.reason })
    }
}

/// `--help`'s prose for `resume-venue`.
pub(super) const RESUME_VENUE_PROSE: &str = "\
Release the LINK DEAD-MAN's halt of ONE venue, early: the engines of VENUE go back to the trading \
state they held before the halt, and nothing else changes. That is what `resume` cannot do: it is \
node-wide, so it also clears every OTHER venue's halt and un-latches the drawdown latch.

VENUE is an exchange id (`bybit`), not a book: the halt is the whole exchange's, every account of it.

The node REFUSES while one of the venue's links is still down (a WARN in its log and a note in its \
recent events; the command still reports accepted, because the verdict is the core's), unless \
--force. A forced resume starts the venue's windows over, so a link that stays down trips again \
after a fresh grace. After a restart the node no longer remembers what the venue held before the \
halt (the overlay is not journaled), and takes the state from its OTHER venues instead; a node whose \
other venues are halted too is a node-wide halt, and `resume` lifts it.

It needs a CONTROL-scope key and a node with `tradehub_control` on, and a node that advertises \
`venue-resume`. `vike-cli trade status` is the READ.

--reason must be LAST - everything after it, to end of line, is taken verbatim as the recorded \
rationale. ALWAYS pass --yes when nothing is at the keyboard (see `trade halt --help`).";

/// `trade resume-venue`'s command line: the venue, `--force`, then the shared one-shot write flags
/// (`--node` is the CONTROL address).
#[derive(clap::Args, Debug)]
pub(super) struct ResumeVenueCmd {
    #[arg(value_name = "VENUE", help = "the exchange whose link-dead-man halt to release")]
    venue: String,
    #[arg(
        long,
        overrides_with = "force",
        help = "resume even though a link of the venue is still down (the node refuses otherwise)"
    )]
    force: bool,
    #[command(flatten)]
    wrap: super::oneshot::WrapperArgs,
}

/// `trade resume-venue VENUE [--force]` through the plane's one write tail: preview, `confirm?
/// [y/N]` (`--yes` skips), send, and the exit rung the node's verdict earns.
///
/// ⚠ The venue is judged here, before a socket opens: a book (`bybit/ALT`, `bybit#ALT`) is refused
/// by name, because the halt is the exchange's and naming a book would read as a narrower request
/// than the node honours.
pub(super) fn run_resume_venue(
    cmd: ResumeVenueCmd,
    keys: &HubKeyring,
    policy_max_notional: Option<f64>,
    ack_wait: Duration,
) -> ExitCode {
    let built = resume_venue_verb(&cmd.venue, cmd.force);
    super::oneshot::run_verb(
        "trade",
        "resume-venue",
        built,
        &cmd.wrap,
        keys,
        policy_max_notional,
        ack_wait,
    )
}

/// The verb `trade resume-venue VENUE [--force]` resolves, or the usage refusal. PURE.
fn resume_venue_verb(venue: &str, force: bool) -> Result<verbs::Verb, crate::exit::CliError> {
    let name = venue.trim();
    if name.is_empty() || name.contains(['/', '#']) {
        return Err(crate::exit::CliError::usage(format!(
            "`{venue}` is not a venue: the link dead-man's halt is the whole EXCHANGE's, so name \
             the exchange (`bybit`), not a book"
        )));
    }
    Ok(verbs::Verb::ResumeVenue { venue: name.to_string(), force })
}

/// `args` read as `trade halt <args…>` by the plane's one parser, then [`ModeCmd::into_args`].
#[cfg(test)]
pub(super) fn parse_mode_args(
    argv: impl IntoIterator<Item = String>,
) -> Result<ModeArgs, args::Stop> {
    let argv = std::iter::once("halt".to_string()).chain(argv);
    match super::parse(argv)?.sub {
        Some(super::Sub::Halt(cmd)) => {
            cmd.into_args().map_err(|msg| args::Stop::message("vike-cli trade halt", msg))
        }
        other => panic!("`trade halt` did not read as the halt verb: {other:?}"),
    }
}

/// Prompt `confirm? [y/N]` and return true only on an explicit `y`/`yes`. Ctrl-C / Ctrl-D / anything
/// else is a "no" (the safe default for an order-write).
fn confirm(rl: &mut DefaultEditor) -> bool {
    is_yes(&rl.readline("confirm? [y/N] ").unwrap_or_default())
}

/// The same prompt read off plain stdin, for the one-shot verbs — see [`run_set_mode_once`] for why
/// they may not use the line editor.
///
/// ⚠ **What this actually does, because the `--help` and the runbook both claimed something
/// stronger and it was false.** They said "a piped stdin reads as NO", which would make an
/// unattended `halt` without `--yes` a guaranteed no-op. It is a plain blocking `read_line`, so a
/// non-terminal stdin has THREE outcomes, not one:
///
/// * **EOF** — a closed stdin, `< /dev/null`, or a systemd unit (whose stdin defaults to `null`):
///   `Ok(0)`, which is a NO. This is the only case the old claim described.
/// * **a line arrives** — `echo y | vike-cli trade halt …` CONFIRMS. A pipe is not a refusal, and
///   a script whose stdin happens to carry a `y` (a heredoc, an inherited descriptor, a `yes |`
///   somebody left in front of the wrong command) halts the node.
/// * **nothing arrives and the writer stays open** — an `ssh host 'vike-cli trade halt …'` with no
///   `-n`, a pipe from a process that has not written yet: this BLOCKS, indefinitely. In an
///   incident that is worse than either answer, and it is the outcome the old wording hid.
///
/// The behaviour is deliberately unchanged: gating on `IsTerminal` would refuse the interactive
/// `ssh host 'vike-cli trade halt …'` an operator actually uses mid-incident (no pty, so stdin is
/// not a terminal there either) to prevent a shape that `--yes` already covers. What changed is
/// that the text now says what the code does. `crates/vike-cli/tests/trade_hub_e2e.rs`'s
/// `a_piped_confirmation_halts_the_node_without_yes` is the pin: it fails the moment a future
/// version makes the old claim true, which is the point at which these words have to change back.
pub(super) fn confirm_stdin() -> bool {
    use std::io::Write;
    print!("confirm? [y/N] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    match std::io::stdin().read_line(&mut answer) {
        Ok(0) | Err(_) => false,
        Ok(_) => is_yes(&answer),
    }
}

/// The ONE place a confirm answer is judged, so the two readers above cannot come to disagree about
/// what counts as yes. Only an explicit `y`/`yes`; everything else — including an empty line — is a
/// no.
pub(super) fn is_yes(answer: &str) -> bool {
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Send one `SetSetting` through the SYNCHRONOUS [`vike_tradehub_client::set_setting`] helper — its
/// own short-lived control connection — and report the node's verdict, including the
/// `restart_required` flag the fire-and-forget path cannot surface.
///
/// It runs AFTER the one `y/N` ([`run_write`]'s), and asks nothing of its own:
/// `docs/decisions/0086` point 7 deleted the retype that used to run here. `file` and `key` are the
/// wire command's own — `file` derived from the key by [`Verb::to_wire_command`], and the `confirm`
/// beside them `None` — and `change` is the `old → new` the preview already printed, repeated with
/// the node's verdict.
pub(super) fn run_set_setting(
    session: &Session<'_>,
    file: &str,
    key: &str,
    value: &str,
    change: &verbs::SettingChange,
    reason: Option<&str>,
) {
    let Some((control_key, _)) = session.keys.control() else {
        // Unreachable: `run_write`'s gate proved a live control connection, which needs a key.
        println!("{}", NoControl::NoKey.refusal_line());
        return;
    };
    // `None`: the node has ignored the wire's `confirm` since 0086 point 7.
    match vike_tradehub_client::set_setting(
        session.node.as_str(),
        control_key.as_bytes(),
        file,
        key,
        value,
        None,
        reason,
    ) {
        Ok(true) => println!(
            "written: {} — applies at the next restart: the RUNNING node keeps its boot-time \
             value until then (every policy.* key is in this class)",
            change.line()
        ),
        Ok(false) => {
            println!("written: {} — applied LIVE by the node, no restart needed", change.line())
        }
        // The three kinds below are the node saying no BEFORE anything was written: a capability
        // it does not advertise, an auth denial, and its own loader refusing the row. Everything
        // else is transport, and transport is where the honest answer is "unknown" — the request
        // may have been written and the reply lost, exactly the asymmetry the order path's
        // `Disconnected` arm reasons about.
        Err(e) => match e.kind() {
            std::io::ErrorKind::Unsupported
            | std::io::ErrorKind::PermissionDenied
            | std::io::ErrorKind::InvalidData => {
                println!("not written: {e} — nothing was written on the node");
            }
            _ => println!(
                "[error] NOT CONFIRMED: {e} — the write may or may not have landed. Re-read that \
                 key on the node before retrying, so a retry cannot be a second write."
            ),
        },
    }
}

/// Read `key`'s CURRENT value off the node and pair it with `value` — the `old → new` a settings
/// write previews ([`verbs::SettingChange`]). One short-lived OBSERVE request
/// ([`vike_tradehub_client::settings_show`]), the same per-call shape [`run_status`]'s registry
/// read uses.
///
/// ⚠ **A read that fails is NOT a refusal.** No observe key, a node too old to serve settings, a
/// transport fault: the write is still the operator's to confirm, against a line that says the old
/// value was not read — rather than no line, which would read as "nothing to show", or a guessed
/// one, which would read as fact.
fn read_setting_change(session: &Session<'_>, key: &str, value: &str) -> verbs::SettingChange {
    let show = match session.keys.observe() {
        Some((observe_key, _)) => {
            vike_tradehub_client::settings_show(session.node.as_str(), observe_key.as_bytes())
                .map_err(|e| e.to_string())
        }
        None => {
            Err(format!("no {} in the environment or the hub-key store", hubkeys::OBSERVE_KEY_ENV))
        }
    };
    verbs::SettingChange::from_show(key, value, show.as_ref().map_err(Clone::clone))
}

#[cfg(test)]
mod resume_venue_tests {
    use super::*;

    /// `trade resume-venue <argv>` through the plane's one parser.
    fn parsed(argv: &[&str]) -> ResumeVenueCmd {
        let argv = std::iter::once("resume-venue").chain(argv.iter().copied()).map(String::from);
        match super::super::parse(argv).expect("parses").sub {
            Some(super::super::Sub::ResumeVenue(c)) => c,
            other => panic!("did not read as `trade resume-venue`: {other:?}"),
        }
    }

    /// The venue, `--force` in either position, and the shared write flags are read; the override
    /// is off unless spelled.
    #[test]
    fn it_reads_the_venue_the_override_and_the_shared_write_flags() {
        let plain = parsed(&["bybit", "--node", "127.0.0.1:1", "--yes"]);
        assert_eq!((plain.venue.as_str(), plain.force, plain.wrap.yes), ("bybit", false, true));
        let forced =
            parsed(&["--force", "bybit", "--node", "127.0.0.1:1", "--reason", "link back"]);
        assert!(forced.force && forced.venue == "bybit");
        assert_eq!(forced.wrap.reason().as_deref(), Some("link back"));
        assert!(
            super::super::parse(["resume-venue".to_string()]).is_err(),
            "the venue is required"
        );
    }

    /// The verb lowers to the VENUE-scoped wire command and never to `SetTradingState` (the whole
    /// reason it exists), is a write, and previews with the override shouted.
    #[test]
    fn it_lowers_to_the_venue_scoped_verb_and_previews_the_override() {
        let verb = verbs::Verb::ResumeVenue { venue: "bybit".into(), force: true };
        assert!(verb.is_write());
        let wire = verb.to_wire_command().expect("a write verb lowers");
        assert_eq!(wire, WireCommand::ResumeVenue { venue: "bybit".into(), force: true });
        let forced = describe_command(&wire);
        assert!(forced.contains("bybit") && forced.contains("FORCE"), "{forced}");
        assert!(forced.contains("ONLY"), "the preview says it is one venue: {forced}");
        let plain =
            describe_command(&WireCommand::ResumeVenue { venue: "bybit".into(), force: false });
        assert!(!plain.contains("FORCE"), "{plain}");
    }

    /// `--help` says what the verb is for and what it does not do, and the REPL has no word for it.
    #[test]
    fn its_help_states_the_scope_and_the_refusal() {
        let help = super::super::help_of(&["resume-venue"]);
        assert!(help.contains("ONE venue") && help.contains("--force"), "{help}");
        assert!(help.contains("drawdown latch"), "it says what `resume` also does: {help}");
        assert!(help.contains("--node"), "the flags `clap` reads: {help}");
        assert!(
            super::super::parse_line("resume-venue bybit").is_err(),
            "no REPL word: it is a one-shot verb, like `recon confirm`"
        );
    }

    /// A book is not a venue: the halt is the whole exchange's, so the one-shot refuses `bybit/ALT`
    /// and `bybit#ALT` (and nothing) by name, as a usage error, before a socket opens.
    #[test]
    fn a_book_is_refused_as_a_venue() {
        for book in ["bybit/ALT", "bybit#ALT", "  "] {
            let err = resume_venue_verb(book, false).expect_err(book);
            assert_eq!(err.exit, crate::exit::Exit::Usage, "{book:?}");
            assert!(err.msg.contains("not a venue"), "{}", err.msg);
        }
        assert_eq!(
            resume_venue_verb(" bybit ", true).expect("a venue"),
            verbs::Verb::ResumeVenue { venue: "bybit".into(), force: true }
        );
    }
}
