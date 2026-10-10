//! The WRITE path: preview + confirm + send, the one-shot `halt`/`resume`, and the settings write.

use std::process::ExitCode;

use rustyline::DefaultEditor;
use vike_tradehub_client::wire::{WireCommand, WireTradingState};
use vike_tradehub_client::{CommandOutcome, ControlRejected, RemoteControlHandle};

use crate::cmd::args::{self, Flags};
use crate::cmd::nodekeys::{self, NodeKeyring};
use crate::cmd::verbs;
use crate::exit::Exit;

use super::describe::describe_command;
use super::{ACK_WAIT, ModeArgs, NoControl, Session};

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
    send_and_report(control, cmd, reason);
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
) -> bool {
    // The ticket is THIS command's identity; its outcome is reported for it and no other.
    match control.try_command_with_reason(cmd, reason) {
        Ok(ticket) => match control.await_outcome(ticket, ACK_WAIT) {
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
                "sent, but the node has not answered within {}s — the outcome is NOT yet known; \
                 check `orders`/`positions` before retrying",
                ACK_WAIT.as_secs()
            ),
        },
        // A CAPABILITY refusal is the one rejection an operator can act on, and `{e:?}` renders it
        // as the bare token `UnsupportedByNode`. It is also the arm the lifecycle verbs make
        // reachable in ordinary use: `mount`/`unmount` need `mount-verbs`, which a node older than
        // split-plane B5 does not advertise, and the refusal happens CLIENT-side — nothing was
        // enqueued and nothing went on the wire.
        Err(ControlRejected::UnsupportedByNode) => println!(
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
    args: impl Iterator<Item = String>,
    keys: &NodeKeyring,
    state: WireTradingState,
) -> ExitCode {
    let verb = mode_verb(state);
    let usage = mode_usage(verb);
    // `--reason` is a rest-of-line tail at the REPL and a flag here — the same value, taken the way
    // each surface's own grammar takes values. The REPL splits it BEFORE the verb grammar sees it
    // (`split_reason`); an argv parser has no line to split.
    let ModeArgs { node, yes, reason } = match parse_mode_args(args) {
        Ok(v) => v,
        Err(msg) => return args::exit_for_parse_error(&format!("trade {verb}"), &usage, &msg),
    };
    let Some(node) = node else {
        eprintln!("vike-cli trade {verb}: --node <host:port> is required\n{usage}");
        return Exit::Usage.into();
    };
    // The CONTROL key specifically: this is a write, and an observe key cannot authenticate one.
    let Some((key, origin)) = keys.control() else {
        eprintln!("vike-cli trade {verb}: {}", NoControl::NoKey.refusal_line());
        return ExitCode::FAILURE;
    };
    let control = match RemoteControlHandle::connect(node.as_str(), key.as_bytes()) {
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
    if send_and_report(&control, cmd, reason) { ExitCode::SUCCESS } else { ExitCode::FAILURE }
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

/// The usage block for one mode verb, rendered from its own name so the two cannot drift.
pub(super) fn mode_usage(verb: &str) -> String {
    let does = if verb == "halt" {
        "Set the node's trading mode to HALTED: the gate refuses every order that OPENS or ADDS \
         risk.\nPosition-covered reduces still pass, so `market-exit` and `flatten` keep working — \
         a halt\nstops trading, it does not trap you."
    } else {
        "Set the node's trading mode back to ACTIVE: ordinary trading resumes."
    };
    format!(
        "usage: vike-cli trade {verb} --node <host:port> [--yes] [--reason <text>]\n\n{does}\n\n\
         It needs a CONTROL-scope key (VIKE_TRADEHUB_OBSERVE_KEY cannot authenticate a write) and \
         a node\nwith `tradehub_control` on. `vike-cli trade status` is the READ — no spelling of \
         it writes.\n\n\
         ⚠ ALWAYS pass --yes when nothing is at the keyboard. Without it this reads one line from \
         stdin:\n  an EOF (a closed stdin, `< /dev/null`, a systemd unit) is a NO and nothing is \
         sent;\n  a `y` on a pipe CONFIRMS — a pipe is not a refusal;\n  \
         a stdin that never delivers a line BLOCKS, which in an incident is worse than either.\n\n\
         options:\n  \
         --node HOST:PORT  the node's control address (required)\n  \
         --yes, -y         skip the confirm? [y/N] prompt (see the warning above)\n  \
         --reason TEXT     operator rationale, recorded in the node's audit trail\n  \
         -h, --help        this message"
    )
}

/// `--node` / `--yes` / `--reason` for the one-shot mode verbs. PURE — unit-tested below.
pub(super) fn parse_mode_args(args: impl Iterator<Item = String>) -> Result<ModeArgs, String> {
    let mut parsed = ModeArgs { node: None, yes: false, reason: None };
    let mut flags = Flags::new(args);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--node" => parsed.node = Some(flags.value(&flag, inline)?),
            "--yes" | "-y" => {
                args::no_value(&flag, inline)?;
                parsed.yes = true;
            }
            "--reason" => parsed.reason = Some(flags.value(&flag, inline)?),
            "-h" | "--help" => return args::help_requested(),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(parsed)
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
/// that the text now says what the code does. `crates/vike-cli/tests/trade_node_e2e.rs`'s
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
        None => Err(format!(
            "no {} in the environment or the node-key store",
            nodekeys::OBSERVE_KEY_ENV
        )),
    };
    verbs::SettingChange::from_show(key, value, show.as_ref().map_err(Clone::clone))
}
