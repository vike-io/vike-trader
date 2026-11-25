//! The WRITE path: preview + confirm + send, the one-shot `halt`/`resume`, and the settings write.

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use rustyline::DefaultEditor;
use vike_model::TradingState;
use vike_tradehub_client::wire::{CommandWire, CoreSnapshotWire};
use vike_tradehub_client::{
    CommandOutcome, ControlRejected, TradehubControlHandle, TradehubCoreHandle, UnansweredCommand,
};

use crate::cmd::args;
use crate::exit::Exit;
use vike_client::first_frame::{stale_frame_warning, wait_for_first_frame};
use vike_client::hubkeys::{self, HubKeyring};
use vike_client::order_gate::{self, NodeLink};
use vike_client::refusals::{OrderGateRefusal, OrderGateRefusalId};
use vike_client::verbs;

use super::describe::{accepted_tail, describe_command};
use super::{ModeArgs, NoControl, Session};

/// The node's frame for `cmd`'s mint and the order gate, or why there is none. Only a verb that names a venue or states
/// a size reads one (`vike_client::order_gate::reads_the_frame`), so `halt` and `cancel` never wait. It waits for the
/// node's FIRST frame: a session's first line can beat it (measured on the CI box), and the client's placeholder shows no
/// account, so a bare venue of several would go on to the preview unrefused.
pub(super) fn write_frame(
    observe: Option<&TradehubCoreHandle>,
    cmd: &CommandWire,
) -> Result<Arc<CoreSnapshotWire>, &'static str> {
    if !order_gate::reads_the_frame(cmd) {
        return Err("this verb names no venue and states no size");
    }
    match observe {
        Some(observe) if observe.is_connected() => Ok(wait_for_first_frame(observe)),
        Some(_) => Err("the observe connection to the node is down"),
        None => Err("this session has no observe connection to the node"),
    }
}

/// **The line a refusal of the one order gate prints** (`vike_client::order_gate::judge`, the rule both doors call):
/// the rendered refusal, `error [<id>]: <text>`, and this door's own tail. The REPL prints it and stays at the prompt;
/// the one-shot verbs exit on the refused rung with it.
pub(super) fn gate_refusal_line(refusal: &OrderGateRefusal) -> String {
    let tail = match refusal.id() {
        OrderGateRefusalId::VenueNotMounted | OrderGateRefusalId::AccountNotMounted => {
            " (`vike-cli trade status` reads what the node runs)"
        }
        OrderGateRefusalId::BareSubmitToOlderNode => {
            ": a book is `<venue>/<LABEL>`, or `<venue>/DEFAULT` for the unlabelled account"
        }
        _ => "",
    };
    format!("{}{tail}", refusal.rendered())
}

/// Preview + confirm + send one WRITE command. The node's server-side gate is the real enforcer.
/// `reason` is the optional operator rationale: shown in the preview and sent BESIDE the command for
/// the node's audit trail — it never becomes part of the order.
///
/// It takes the resolved [`CommandWire`] rather than a [`Verb`] because everything below this
/// point is about the wire form: the order client id mint, the preview line, the guardrail and the settings
/// branch all read the command's VARIANT. Both grammars resolve through the one
/// [`Verb::to_wire_command`] before they get here.
pub(super) fn run_write(
    session: &mut Session<'_>,
    rl: &mut DefaultEditor,
    cmd: CommandWire,
    reason: Option<String>,
) {
    // MINT the order-client-id before anything is printed. Two reasons, both load-bearing: the node
    // REFUSES a remote submit with an empty one (so an unminted command can never be placed), and
    // the preview must show the id that will actually be sent — it is what the operator reads off
    // the screen and types back at `cancel <order client id>`. An explicit `--order-client-id` is left alone. The frame gives the id the
    // shape and account code of the block the order reaches, and its intent.
    let frame = write_frame(session.observe.as_ref(), &cmd);
    let control = session.control.as_ref();
    let cmd = verbs::fill_order_client_id(
        cmd,
        &mut session.order_client_ids,
        frame.as_deref().ok(),
        |c| control.is_some_and(|h| h.takes(c)),
    );
    // THE ONE ORDER GATE, the MCP's too: a venue the frame shows the node does not run, a bare venue of several
    // accounts (decision 0254), a node that cannot route it, and an order over this box's notional cap are NOT SENT,
    // printed alone, before the preview.
    let link = session.control.as_ref().map_or_else(NodeLink::default, NodeLink::of);
    let frame_or_why = frame.as_deref().map_err(|why| *why);
    let guard = match order_gate::judge(&cmd, session.capabilities, frame_or_why, link) {
        Ok((guard, _)) => guard,
        Err(refusal) => {
            println!("{}", gate_refusal_line(&refusal));
            return;
        }
    };
    // PREVIEW — always, even in --yes mode, so the human sees exactly what will be sent, with the
    // gate's guardrail (the line the mcp tools' preview carries as JSON).
    println!("preview: {}", describe_command(&cmd));
    println!("  {}", guard.line());
    // The frame the gate just judged on, when it is old: the rule the MCP preview says it by (`frame_stale`).
    let age = session.observe.as_ref().and_then(TradehubCoreHandle::frame_age);
    if let Some(w) = age.filter(|_| frame.is_ok()).and_then(stale_frame_warning) {
        println!("  ⚠ {w}");
    }
    // An order client id this REPL could not have minted and would have REFUSED on `submit`. Advisory: it is
    // still sent (see [`verbs::order_client_id_charset_warning`] for why refusing would be wrong).
    if let Some(w) = verbs::order_client_id_charset_warning(&cmd) {
        println!("  {w}");
    }
    if let Some(why) = &reason {
        println!("  reason (recorded in the node's audit trail): {why}");
    }
    if let Some(note) = super::oneshot::reason_scope_note(&cmd, reason.as_deref()) {
        println!("  {note}");
    }
    // A SETTINGS write shows what it CHANGES before the y/N: `old → new`, the old value read off
    // the node this write is aimed at. That line, the node's own bounds check and its journal are
    // what guard a live limit since `docs/decisions/0086` point 7 took the retype away.
    let change = match &cmd {
        CommandWire::SetSetting { key, value, .. } => {
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
            // NAME THE ACTUAL REASON. This used to be one hard-coded no-key line for all three
            // causes; with the node's control flag off and a valid key it was simply false, and the
            // true reason had already scrolled past at connect time.
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
    // the shared worker maps an accepted `SetSetting` to an empty-order-client-id `Accepted` and DROPS the
    // reply's `restart_required`, so this path would have to tell the operator "accepted" while
    // knowing nothing about whether the value is live — the one question a settings write is asked.
    // `change` is `Some` exactly for this variant: it was built from the same match above.
    if let (CommandWire::SetSetting { file, key, value, .. }, Some(change)) = (&cmd, &change) {
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

/// Send ONE resolved [`CommandWire`] on an open control connection and report the node's verdict on
/// THAT command. Returns the rung the verdict earns — [`Exit::Ok`] when the node ACCEPTED it,
/// [`Exit::OutcomeUnknown`] when it was sent and never answered, [`Exit::Connect`] when the node
/// answered NOT NOW (still starting, its core busy) or nothing of it was sent (a link already closed,
/// a full queue), [`Exit::Failed`] otherwise — which the one-shot verbs exit with and the REPL ignores
/// (a REPL has no exit code, only the printed line).
///
/// ⚠ Extracted from [`run_write`] when `vike-cli trade halt` / `resume` became one-shot verbs
/// (ruling 17), and extracted rather than re-rolled for the reason the outcome arms below spell out
/// at length: the difference between `Disconnected` ("it may or may not have executed") and
/// `NeverSent` ("nothing went to the node") is the whole safety content of this surface, and a
/// second copy of that reasoning is a second copy to get wrong. It never polls
/// [`TradehubControlHandle::last_error`], which LATCHES — see this module's doc for what that cost.
pub(super) fn send_and_report(
    control: &TradehubControlHandle,
    cmd: CommandWire,
    reason: Option<String>,
    ack_wait: Duration,
) -> Exit {
    // An Ack is the command's receipt, not what the core then does with it (see `accepted_tail`).
    let tail = accepted_tail(&cmd, "orders", "positions");
    // The ticket is THIS command's identity; its outcome is reported for it and no other.
    match control.try_command_with_reason(cmd, reason) {
        Ok(ticket) => match control.await_outcome(ticket, ack_wait) {
            Some(CommandOutcome::Accepted { order_client_id, notes }) => {
                let named = if order_client_id.is_empty() {
                    String::new()
                } else {
                    format!(" (order client id {order_client_id})")
                };
                println!("accepted by the node{named} — {tail}");
                // What the node says about a command it SENT (a closed market, a flatten of nothing).
                for note in &notes {
                    println!("  note: {note}");
                }
                return Exit::Ok;
            }
            // NOT NOW (a node still starting, its core busy): nothing executed, so the one-shot
            // `halt`/`resume` exit on the retry rung, as `oneshot::execute_write`'s writes do.
            Some(CommandOutcome::NotNow(refusal)) => {
                println!("{refusal}");
                return Exit::Connect;
            }
            Some(CommandOutcome::Refused(refusal)) => {
                println!("node rejected the command: {refusal}")
            }
            // The connection died before the reply: genuinely UNKNOWN, so say so. Printing "sent"
            // here (what the old latch-poll did, since it saw no error) is the same class of lie as
            // the stale refusal, pointing the other way.
            Some(CommandOutcome::Disconnected) => {
                println!(
                    "[error] {} Reconnect and read `orders`/`positions`.",
                    UnansweredCommand::LinkDropped
                );
                return Exit::OutcomeUnknown;
            }
            // NOT the same statement as the arm above, and the difference is what the operator
            // needs: the link was already closed when the sender reached this command, so nothing
            // was written and nothing can have executed. Safe to re-issue verbatim once the
            // session is reconnected — no book check required first. (This REPL holds one handle
            // for the session and does not reconnect by itself, deliberately: `trade` is a human
            // at a prompt who can see this line and re-run the command, unlike the MCP server,
            // whose caller is an agent mid-tool-call.)
            Some(CommandOutcome::NeverSent) => {
                println!(
                    "{} Reconnect (re-run `vike-cli trade`) and issue it again.",
                    UnansweredCommand::LinkAlreadyClosed
                );
                return Exit::Connect;
            }
            None => {
                println!(
                    "{} Read `orders`/`positions`.",
                    UnansweredCommand::NoAnswerWithin(ack_wait)
                );
                return Exit::OutcomeUnknown;
            }
        },
        // A CAPABILITY refusal is the one rejection an operator can act on, and `{e:?}` renders it
        // as the bare token `UnsupportedByHub`. It is also the arm the lifecycle verbs make
        // reachable in ordinary use: `mount`/`unmount` need `mount-verbs`, which a node older than
        // split-plane B5 does not advertise, and the refusal happens CLIENT-side — nothing was
        // enqueued and nothing went on the wire.
        Err(rejected @ ControlRejected::UnsupportedByHub) => {
            println!("{rejected} Upgrade the node, or use a verb it speaks.");
        }
        // NOT SENT, on the retry rung as a never-sent is: nothing was enqueued for this command.
        Err(rejected @ ControlRejected::Gone) => {
            println!("{rejected} Reconnect (re-run `vike-cli trade`) and issue it again.");
            return Exit::Connect;
        }
        Err(rejected @ ControlRejected::Busy) => {
            println!("{rejected}");
            return Exit::Connect;
        }
    }
    // What is left is a decided NOT-accepted outcome: the node said no, or the node lacks the verb.
    Exit::Failed
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
/// unconfirmed command / a node that answered the control connect and refused it (its scope, an older
/// node, a version skew), `Exit::Connect` when the control connection could not be opened at all, the
/// node answered NOT NOW or nothing of the write was sent (a link already closed, a full queue),
/// `Exit::OutcomeUnknown` when the write was sent and never answered, and
/// SUCCESS only when the node
/// ACCEPTED the write. An aborted confirm is a FAILURE rather than a
/// success: nothing was sent, and a wrapper must not read "the operator said no" as "the node is
/// halted".
pub(super) fn run_set_mode_once(
    cmd: ModeCommand,
    keys: &HubKeyring,
    state: TradingState,
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
    let node = match super::node_or_refusal(node, keys.node_addr()) {
        Ok(node) => node,
        Err(why) => return args::refuse(&bin, why),
    };
    // The CONTROL key specifically: this is a write, and an observe key cannot authenticate one.
    let Some((key, origin)) = keys.control() else {
        eprintln!("vike-cli trade {verb}: {}", NoControl::NoKey.refusal_line());
        return ExitCode::FAILURE;
    };
    let control = match TradehubControlHandle::connect_with_reply_timeout(
        node.as_str(),
        key,
        keys.patience().control_reply,
    ) {
        Ok(h) => h,
        Err(e) => {
            let cause = NoControl::from_connect_error(&e);
            eprintln!("vike-cli trade {verb}: {}", cause.refusal_line());
            // A node that ANSWERED (a refused scope, an older node, a version skew) is a
            // configuration fact about a reachable node, not a socket to retry — the
            // `trade::status` ladder's rule, applied to the write side.
            return cause.exit().into();
        }
    };
    println!("connected: CONTROL (write) to {node} [key from the {}]", origin.label());

    let cmd = CommandWire::SetTradingState(state);
    println!("preview: {}", describe_command(&cmd));
    if let Some(why) = &reason {
        println!("  reason (recorded in the node's audit trail): {why}");
    }
    if !yes && !confirm_stdin(false) {
        println!("aborted — nothing was sent");
        return ExitCode::FAILURE;
    }
    send_and_report(&control, cmd, reason, ack_wait).into()
}

/// The word the operator typed, for every message this one-shot prints. Derived from the STATE so
/// the two can never disagree — a message naming `halt` while sending `Active` is the exact class
/// of confusion ruling 17 exists to remove.
pub(super) fn mode_verb(state: TradingState) -> &'static str {
    match state {
        TradingState::Halted => "halt",
        TradingState::Active => "resume",
        // Unreachable: no verb produces `Reducing` — it has no CLI spelling by ruling 17, and is
        // reached through `halt`'s covered-reduce behaviour (this module's doc). Kept total rather
        // than `unreachable!`, because a panic in an order surface is never the better answer.
        TradingState::Reducing => "reduce",
    }
}

/// `--help`'s prose for one mode verb (the `usage:` line and the options table are `clap`'s),
/// rendered from the verb's own name so the two cannot drift.
pub(super) fn mode_prose(verb: &str) -> String {
    let does = if verb == "halt" {
        "Set the node's trading mode to HALTED: the gate refuses every order that OPENS or ADDS \
         risk.\nPosition-covered reduces still pass, so `close-all` and `flatten` keep working — \
         a halt\nstops trading, it does not trap you."
    } else {
        "Set the node's trading mode back to ACTIVE: ordinary trading resumes.\n\n\
         ⚠ It is NODE-WIDE and REPLACES every account's state, including an account you halted or \
         reduced on its own with `trade account`: that account is REOPENED too, and the node \
         names the route keys it reopened in its log and its recent events. The one thing it does \
         not lift is the closing-only hold of a lane twin the node keeps up to be closed. To resume \
         ONE account and leave the others as they are, use `trade account resume <account>`."
    };
    format!(
        "{does}\n\n\
         It needs a device whose role admits control (an observe role cannot authenticate a \
         write) and a node\nwith `tradehub_control` on. `vike-cli trade status` is the READ — no spelling of \
         it writes.\n\n\
         ⚠ ALWAYS pass --yes when nothing is at the keyboard. Without it this reads one line from \
         stdin:\n  an EOF (a closed stdin, `< /dev/null`, a systemd unit) is a NO and nothing is \
         sent;\n  a `y` on a pipe CONFIRMS — a pipe is not a refusal;\n  \
         a stdin that never delivers a line BLOCKS, which in an incident is worse than either."
    )
}

// The command line of the one-shot mode verbs `halt` and `resume`, as `clap` reads it. `--node`
// is required, but that is judged after the parse ([`ModeCommand::into_args`]) so `--help` never has
// to supply one.
//
// `--reason` takes exactly ONE value here (unlike the write verbs' rest-of-argv `--reason`,
// `crate::cmd::trade::oneshot::WrapperArgs`): a value beginning with a single dash is a value
// (`--reason -urgent`), and one beginning with `--` is refused by [`ModeCommand::into_args`].
#[derive(clap::Args, Debug)]
pub(super) struct ModeCommand {
    #[arg(
        long,
        value_name = "HOST:PORT",
        overrides_with = "node",
        help = "the node's control address [default: config.node_addr]"
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

impl ModeCommand {
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

// History, kept so a citation by symbol still lands: `RESUME_VENUE_PROSE`, `ResumeVenueCommand`,
// `run_resume_venue` and `resume_venue_verb` were the group-less `trade resume-venue`; the verb is
// `trade venue resume` now: `crate::cmd::trade::venue`'s `RESUME_PROSE`, `ResumeCommand`, `run_resume`
// and `resume_verb`.

// History, kept so a citation by symbol still lands: `run_mount_unlatch` was the one-shot `trade
// mount-unlatch`; the verb is `trade strategy reset-budget` now, in `crate::cmd::trade::strategy`.

/// `args` read as `trade halt <args…>` by the plane's one parser, then [`ModeCommand::into_args`].
#[cfg(test)]
pub(super) fn parse_mode_args(
    argv: impl IntoIterator<Item = String>,
) -> Result<ModeArgs, args::Stop> {
    parse_trade_verb!(argv, "halt", Halt, "vike-cli trade halt")
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
/// that the text now says what the code does. `crates/vike-cli/tests/shipped_cli/trade_hub_e2e.rs`'s
/// `a_piped_confirmation_halts_the_node_without_yes` is the pin: it fails the moment a future
/// version makes the old claim true, which is the point at which these words have to change back.
///
/// `on_stderr` is for a `--json` run, whose stdout is one JSON document and nothing else.
pub(super) fn confirm_stdin(on_stderr: bool) -> bool {
    use std::io::Write;
    if on_stderr {
        eprint!("confirm? [y/N] ");
        let _ = std::io::stderr().flush();
    } else {
        print!("confirm? [y/N] ");
        let _ = std::io::stdout().flush();
    }
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
        control_key,
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
        // Only a failure AFTER the command left is "unknown" (`HubCallError::OutcomeUnknown`): the
        // request may have been written and the reply lost, exactly the asymmetry the order path's
        // `Disconnected` arm reasons about, said in the one wording. A dial or handshake that failed,
        // a capability the node does not advertise, an auth denial, a node still starting and its
        // loader refusing the row all wrote nothing — the same reading MCP's settings write makes.
        Err(e) => match vike_tradehub_client::UnansweredCommand::of_settings_write(&e) {
            Some(unanswered) => {
                println!("[error] {unanswered} ({e}) Re-read that key on the node before retrying.")
            }
            None => println!("not written: {e} — nothing was written on the node"),
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
            vike_tradehub_client::settings_show(session.node.as_str(), observe_key)
                .map_err(|e| e.to_string())
        }
        None => Err(format!("no {} in this box's settings database", hubkeys::DEVICE_KEY_WORDS)),
    };
    verbs::SettingChange::from_show(key, value, show.as_ref().map_err(Clone::clone))
}
