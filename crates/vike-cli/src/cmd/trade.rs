//! `vike-cli trade` — a HUMAN interactive REPL over a running vike-tradehub node (headless two-layer
//! plan, Layer 1). The terse, terminal-driven sibling of the `mcp` trade tools: a person types short
//! verbs (`submit`, `cancel`, `flatten`, `orders`, `positions`, `state`, …) and this dispatches them
//! to the node's authenticated OBSERVE (read) and CONTROL (write) servers via the LIGHT wire crate
//! (`vike_tradehub_client::{RemoteCoreHandle, RemoteControlHandle}`). Part of the "vike-cli as the one
//! agent surface" program (`docs/superpowers/specs/2026-07-26-vike-cli-agent-surface-program.md`).
//!
//! # The two halves (each gated on a node key)
//!
//! - READ needs `VIKE_TRADEHUB_OBSERVE_KEY` → a [`RemoteCoreHandle`] subscribed to the node's pushed
//!   [`WireSnapshot`] stream. Absent ⇒ the read verbs report reads are disabled.
//! - WRITE needs `VIKE_TRADEHUB_CONTROL_KEY` → a [`RemoteControlHandle`] the write verbs push
//!   [`WireCommand`]s over. Absent ⇒ OBSERVE-ONLY (write verbs are refused with a clear message).
//! - Neither key present ⇒ a clean startup error (there is nothing to do) that NAMES both places it
//!   looked, including the credential store's path.
//!
//! Both keys are resolved ONCE by the dispatcher and arrive as a [`NodeKeyring`]: the process
//! environment first, then `<project>/settings/secrets.env` — the same credential store
//! `vike-tradehub` itself loads them from. ⚠ This file used to read `std::env::var` and NOTHING
//! else, so a box with both keys in the store (the daemon's own documented configuration, with
//! `vike-cli secrets list` confirming them) got "nothing to do" and an exit.
//! [`crate::cmd::nodekeys`]' module doc argues the precedence.
//!
//! # Every order carries a coid THIS side minted
//!
//! A node REFUSES a remote `Submit` with an empty `client_order_id` (`vike_tradehub::server`'s
//! `lower_command`: a remote peer whose id the runtime minted has no stable handle to cancel or
//! dedup by). [`parse_submit`] used to leave it empty "for the runtime to mint" — an in-process
//! assumption the wire has rejected since that rule landed, so **the REPL could never place an
//! order at all**. It is now filled from the session's `vike_model::ClientOrderIdGenerator`
//! ([`verbs::fill_client_order_id`]) BEFORE the preview prints, so the id shown is the id sent and
//! the id the operator types back at `cancel`. `submit … --coid <id>` pins one explicitly.
//!
//! # The write guardrail (advisory — the node is the enforcing gate)
//!
//! Every WRITE verb PREVIEWS first: it prints the resolved [`WireCommand`] plus a client-side
//! guardrail check over this machine's `policy.toml` notional ceiling (`max_notional_per_order` —
//! it was `VIKE_MAX_ORDER_NOTIONAL` until settings-unification Phase 5) and the local
//! `VIKE_MAX_ORDER_QTY` typo-catcher, then asks `confirm? [y/N]`
//! (skipped with `--yes`). Only on `y` is the command sent, and we then wait briefly for the node's
//! verdict on THAT command — [`RemoteControlHandle::await_outcome`] on the ticket the send returned
//! — and report accepted / refused / unknown. The node's
//! server-side `ControlLimits` (notional + rate) and the core `RiskGate` are the ENFORCING backstop —
//! the client preview is UX, the node gate is truth (exactly as documented for the `mcp` tools).
//!
//! ⚠ This used to poll `RemoteControlHandle::last_error`, which LATCHES: after one refusal every
//! later write in the session printed that same refusal, including commands the node accepted and
//! ran. An operator who fat-fingered an order, got a correct refusal, then typed `market-exit` was
//! told the panic button was rejected when it had executed. Report a command's outcome from ITS
//! ticket; never from a shared side-channel.
//!
//! # `--reason` (node proto v4)
//!
//! Any line may end with `--reason <text to end of line>`: an operator RATIONALE recorded in the
//! node's audit trail beside the command, so an incident review reads *why*, not just *what*. It is
//! split off the line BEFORE the verb grammar sees it ([`split_reason`]), shown in the preview, and
//! sent beside the [`WireCommand`] — it never becomes part of the order. The text runs to end of
//! line (no quoting needed; one optional layer of surrounding quotes is stripped), and the node
//! sanitizes it before recording (control characters stripped, capped at 512 chars).
//!
//! # What is / is not tested
//!
//! The PURE parts are unit-tested: [`parse_line`] (every verb round-trips, malformed input is
//! rejected, `is_write` is correct) and the [`Verb::to_wire_command`] mapping. The REPL loop,
//! `rustyline`, and the live connection are thin glue over those pure parts (they need a live node +
//! a TTY), so they are not unit-tested.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use vike_model::client_order_id::ClientOrderIdGenerator;
use vike_tradehub_client::wire::{WireCommand, WireOrderRequest, WireSnapshot, WireTradingState};
use vike_tradehub_client::{CommandOutcome, RemoteControlHandle, RemoteCoreHandle};

use crate::cmd::args::{self, Flags};
use crate::cmd::nodekeys::{self, NodeKeyring};
use crate::cmd::verbs;
use crate::cmd::verbs::Verb;

const USAGE: &str = "usage: vike-cli trade --node <host:port> [--yes]";

/// The prompt shown by the REPL.
const PROMPT: &str = "vike> ";

/// How long to wait for the node's first pushed frame before a read verb gives up on `seq 0`.
const FIRST_FRAME_WAIT: Duration = Duration::from_secs(2);

/// How long to wait for the node's verdict on the command we just sent before reporting it as not
/// yet known. This is a DEADLINE, not a sleep: `await_outcome` returns the instant that command's
/// reply lands (sub-millisecond on localhost), so a generous bound costs nothing in the common case
/// and buys a definite answer over a slow link. It replaced an unconditional 150ms sleep, which was
/// both slower on every write and too short for a tunnelled node.
const ACK_WAIT: Duration = Duration::from_secs(2);

/// The parsed `trade` command line.
struct Config {
    node: Option<String>,
    /// Skip the per-write `confirm? [y/N]` prompt.
    yes: bool,
    help: bool,
}

/// Entry point the dispatcher routes to. `args` is everything AFTER the `trade` subcommand;
/// `policy_max_notional` is this machine's `max_notional_per_order` policy ceiling, already
/// resolved by [`crate::run`] (settings unification, Phase 5 — it replaced the removed
/// `VIKE_MAX_ORDER_NOTIONAL`) and used only by the advisory preview guardrail.
///
/// `settings_dir` is `<project>/settings`, likewise resolved ONCE by [`crate::run`] from its single
/// `std::env::vars()` sweep. [`history_file`] hangs the REPL history off its `state/`
/// sub-directory. It arrives as a PARAMETER because this file is a library file: a read here would
/// be a `Layer::Library` row on the settings registry's STEP-2 work-list for a value the dispatcher
/// was already positioned to resolve. `None` (no project above the working directory) means history
/// is simply not persisted.
///
/// `keys` is the resolved [`NodeKeyring`] — the two HMAC node keys, taken from the process
/// environment and, failing that, from the credential store the daemon itself reads. Same
/// reasoning: the dispatcher owns both reads, this file takes the answer.
pub fn run(
    args: impl Iterator<Item = String>,
    policy_max_notional: Option<f64>,
    settings_dir: Option<std::path::PathBuf>,
    keys: &NodeKeyring,
) -> ExitCode {
    let cfg = match parse_config(args) {
        Ok(c) => c,
        Err(msg) => {
            eprintln!("vike-cli trade: {msg}\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    if cfg.help {
        println!("{USAGE}\n");
        print_verb_help();
        return ExitCode::SUCCESS;
    }
    let Some(node) = cfg.node else {
        eprintln!("vike-cli trade: --node <host:port> is required\n{USAGE}");
        return ExitCode::FAILURE;
    };

    // Connect whichever half resolved a key. Absent OBSERVE key = no reads; absent CONTROL key =
    // observe-only; NEITHER = a clean error that names both places we looked (the store included —
    // the old message claimed "not set in the environment" while the daemon's own keys sat in the
    // credential store, unread).
    if !keys.has_any() {
        eprintln!("{}", keys.absent_message("trade"));
        return ExitCode::FAILURE;
    }

    let observe = match keys.observe() {
        Some((key, origin)) => match RemoteCoreHandle::connect(node.as_str(), key.as_bytes()) {
            Ok(h) => {
                println!("connected: OBSERVE (read) to {node} [key from the {}]", origin.label());
                Some(h)
            }
            Err(e) => {
                eprintln!("vike-cli trade: cannot open observe connection to {node}: {e}");
                None
            }
        },
        None => {
            println!("reads disabled: no {}", nodekeys::OBSERVE_KEY_ENV);
            None
        }
    };
    // The control half is a THREE-way outcome, not two, and the third is the one a clean install
    // tripped over — see [`NoControl`]. `control_denied` carries the connect-time reason forward so
    // that every later refusal can name it; without it, "the node refused the control SCOPE" and
    // "you have no key" printed the same sentence, and only one of them was true.
    let mut control_denied: Option<NoControl> = None;
    let control = match keys.control() {
        Some((key, origin)) => match RemoteControlHandle::connect(node.as_str(), key.as_bytes()) {
            Ok(h) => {
                println!("connected: CONTROL (write) to {node} [key from the {}]", origin.label());
                Some(h)
            }
            Err(e) => {
                eprintln!("vike-cli trade: cannot open control connection to {node}: {e}");
                control_denied = Some(NoControl::from_connect_error(&e));
                None
            }
        },
        None => {
            println!("writes disabled (OBSERVE-ONLY): no {}", nodekeys::CONTROL_KEY_ENV);
            control_denied = Some(NoControl::NoKey);
            None
        }
    };
    if observe.is_none() && control.is_none() {
        eprintln!("vike-cli trade: no usable connection to {node} — exiting");
        return ExitCode::FAILURE;
    }

    let mut session = Session {
        observe,
        control,
        control_denied,
        yes: cfg.yes,
        caps: verbs::guardrail_caps(policy_max_notional),
        history: history_file(settings_dir.as_deref()),
        coids: verbs::coid_minter(),
    };
    match repl(&mut session) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vike-cli trade: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Parse `--node <host:port>` (required for a session), `--yes` (skip write confirmation), and
/// `--help`/`-h`. Accepts both `--flag value` and `--flag=value` via the shared
/// [`crate::cmd::args`] glue. Unlike the non-interactive commands, `-h`/`--help` here records
/// `help = true` (this command has a real verb-reference screen) rather than erroring.
fn parse_config(args: impl Iterator<Item = String>) -> Result<Config, String> {
    let mut node: Option<String> = None;
    let mut yes = false;
    let mut help = false;
    let mut flags = Flags::new(args);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--node" => node = Some(flags.value(&flag, inline)?),
            "--yes" | "-y" => {
                args::no_value(&flag, inline)?;
                yes = true;
            }
            "-h" | "--help" => help = true,
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(Config { node, yes, help })
}

/// **Why this session has no control connection.** Recorded at connect time because that is the only
/// moment the answer exists, and read at WRITE time, which is the only moment anybody asks.
///
/// The defect this exists to fix: `run_write` used to branch on `session.control.is_none()` alone
/// and print `no VIKE_TRADEHUB_CONTROL_KEY — nothing was sent`. With `tradehub_control` off at the
/// node and a perfectly good key in the credential store, that sentence is FALSE — the key resolved,
/// authenticated as a credential, and was refused for its SCOPE (`server.rs`'s `run_handshake`:
/// "control disabled on this node"). The true reason was printed once, at connect, then dropped;
/// every write afterwards sent the operator hunting for a key that was never the problem.
///
/// The three cases are genuinely different actions — put a key in the store / turn the node's flag
/// on / fix the network — so they are three variants rather than one string with a comment.
#[derive(Debug, Clone, PartialEq, Eq)]
enum NoControl {
    /// No control key resolved at all: neither the process environment nor the credential store had
    /// one. THIS is the case the old message described, and the only one it was ever right about.
    NoKey,
    /// A key resolved and the node REFUSED the control scope — `flags.toml`'s `tradehub_control`
    /// (or `VIKE_TRADEHUB_CONTROL`) is off there, so the daemon zeroes its control key before the
    /// server ever sees it and no key can verify. Carries the node's own words.
    Refused(String),
    /// The connection did not get far enough to be refused — wrong address, node down, TLS/socket
    /// error. Nothing about the key or the node's policy is known.
    Unreachable(String),
}

impl NoControl {
    /// Classify a failed `RemoteControlHandle::connect`.
    ///
    /// `PermissionDenied` is the kind `node_handshake` maps `Response::AuthDenied` to, and it is the
    /// ONLY authenticated-and-refused outcome — everything else is transport. Matching the KIND
    /// rather than the message text is what keeps this from breaking when the node rewords its
    /// reason, which it is free to do.
    fn from_connect_error(e: &std::io::Error) -> Self {
        let msg = e.to_string();
        match e.kind() {
            std::io::ErrorKind::PermissionDenied => NoControl::Refused(msg),
            _ => NoControl::Unreachable(msg),
        }
    }

    /// The line printed instead of sending a write. Always ends in "nothing was sent", because that
    /// is the one fact every case shares and the one the operator most needs.
    fn refusal_line(&self) -> String {
        match self {
            NoControl::NoKey => format!(
                "writes disabled (OBSERVE-ONLY): no {} in the environment or the credential store \
                 — nothing was sent",
                nodekeys::CONTROL_KEY_ENV
            ),
            NoControl::Refused(why) => format!(
                "writes disabled: the node REFUSED the control scope at connect time ({why}). Your \
                 {} is fine — it authenticated; the node is not accepting control connections at \
                 all. Turn `tradehub_control` on in ITS settings/flags.toml (or \
                 VIKE_TRADEHUB_CONTROL=1) and restart it — nothing was sent",
                nodekeys::CONTROL_KEY_ENV
            ),
            NoControl::Unreachable(why) => format!(
                "writes disabled: the control connection could not be opened ({why}) — nothing was \
                 sent"
            ),
        }
    }
}

/// The live REPL session: the two optional node connections, the confirm-skip flag, the
/// advisory preview caps (resolved once at startup — see [`verbs::guardrail_caps`]), and where the
/// line history is persisted.
struct Session {
    observe: Option<RemoteCoreHandle>,
    control: Option<RemoteControlHandle>,
    /// Why [`Self::control`] is `None`, captured at connect time. `None` here means the control
    /// connection is UP. See [`NoControl`].
    control_denied: Option<NoControl>,
    yes: bool,
    caps: verbs::GuardrailCaps,
    /// `<project>/settings/state/trade_history`, or `None` when no project sits above the working
    /// directory (history is then simply not persisted). See [`history_file`].
    history: Option<std::path::PathBuf>,
    /// This session's client-order-id generator. SESSION-scoped, not per-order: the 8-hex prefix
    /// separates this REPL from every other process (and from its own previous run), the counter
    /// separates each order from the last. See [`verbs::coid_minter`].
    coids: ClientOrderIdGenerator,
}

/// The interactive loop. Reads a line, parses a [`Verb`], dispatches it. Ctrl-C cancels the current
/// line (continue), Ctrl-D quits. History persists under the settings directory's `state/`
/// (best-effort).
fn repl(session: &mut Session) -> Result<(), String> {
    let mut rl = DefaultEditor::new().map_err(|e| format!("cannot start the line editor: {e}"))?;
    let history = session.history.clone();
    if let Some(path) = &history {
        let _ = rl.load_history(path); // best-effort — a missing file is fine on first run
    }
    println!("type `help` for commands, `quit` (or Ctrl-D) to exit");

    loop {
        match rl.readline(PROMPT) {
            Ok(line) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let _ = rl.add_history_entry(trimmed);
                // The optional `--reason <text>` tail is split off FIRST — it is audit metadata,
                // not part of the verb grammar, so the parser never has to know about it.
                let (line, reason) = split_reason(trimmed);
                match parse_line(line) {
                    Ok(Verb::Quit) => break,
                    Ok(verb) => dispatch(session, &mut rl, verb, reason),
                    Err(msg) => println!("error: {msg} (type `help`)"),
                }
            }
            // Ctrl-C: abandon this line, keep the REPL running.
            Err(ReadlineError::Interrupted) => continue,
            // Ctrl-D / EOF: quit.
            Err(ReadlineError::Eof) => break,
            Err(e) => return Err(format!("line editor error: {e}")),
        }
    }

    if let Some(path) = &history {
        // `state/` is created lazily, on the first save — a session that never runs must not leave
        // an empty directory behind. Best-effort throughout: history is convenience, not data.
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = rl.save_history(path); // best-effort
    }
    Ok(())
}

/// Run one parsed verb against the live session. Reads print an aligned table from the latest
/// snapshot; writes preview + confirm + send. `reason` is the optional `--reason` rationale, which
/// only a WRITE carries (a read has no audit record to attach it to).
fn dispatch(session: &mut Session, rl: &mut DefaultEditor, verb: Verb, reason: Option<String>) {
    if verb.is_write() {
        run_write(session, rl, verb, reason);
        return;
    }
    if reason.is_some() {
        println!("note: --reason is only recorded for WRITE commands; ignored here");
    }
    // Reads (and the read half of `state` / `help`).
    match verb {
        Verb::Help => print_verb_help(),
        Verb::Orders(symbol) => with_snapshot(session, |s| print_orders(s, symbol.as_deref())),
        Verb::Positions(venue) => with_snapshot(session, |s| print_positions(s, venue.as_deref())),
        Verb::Equity => with_snapshot(session, print_equity),
        Verb::Snapshot => with_snapshot(session, print_snapshot),
        Verb::Recent(n) => with_snapshot(session, |s| print_recent(s, n)),
        Verb::ShowState => with_snapshot(session, print_state),
        // Writes are handled above; Quit is handled in the loop.
        _ => {}
    }
}

/// Fetch the freshest snapshot (waiting briefly for the node's first frame) and hand it to `f`.
/// Reports cleanly when the observe half is disabled or disconnected.
fn with_snapshot(session: &Session, f: impl FnOnce(&WireSnapshot)) {
    let Some(observe) = session.observe.as_ref() else {
        println!(
            "reads disabled: no {} — start with an observe key to read node state",
            nodekeys::OBSERVE_KEY_ENV
        );
        return;
    };
    // A freshly-subscribed connection returns the empty placeholder (seq 0) until the node pushes
    // its first coalesced frame; wait up to FIRST_FRAME_WAIT for a real one.
    let deadline = Instant::now() + FIRST_FRAME_WAIT;
    let mut snap = observe.snapshot();
    while snap.seq == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        snap = observe.snapshot();
    }
    if !observe.is_connected() {
        println!("[warning] observe connection is down — showing the last snapshot seen");
    }
    f(&snap);
}

/// Preview + confirm + send one WRITE verb. The node's server-side gate is the real enforcer.
/// `reason` is the optional operator rationale: shown in the preview and sent BESIDE the command for
/// the node's audit trail — it never becomes part of the order.
fn run_write(session: &mut Session, rl: &mut DefaultEditor, verb: Verb, reason: Option<String>) {
    let cmd = match verb.to_wire_command() {
        Some(c) => c,
        None => {
            println!("error: not a write command");
            return;
        }
    };
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

    let Some(control) = session.control.as_ref() else {
        // NAME THE ACTUAL REASON. This used to be one hard-coded "no VIKE_TRADEHUB_CONTROL_KEY"
        // line for all three causes; with the node's control flag off and a valid key in the store
        // it was simply false, and the true reason had already scrolled past at connect time.
        println!(
            "{}",
            session
                .control_denied
                .as_ref()
                .map_or_else(|| NoControl::NoKey.refusal_line(), NoControl::refusal_line)
        );
        return;
    };
    if !control.is_connected() {
        println!("[error] control connection is down — nothing was sent");
        return;
    }

    if !session.yes && !confirm(rl) {
        println!("aborted — nothing was sent");
        return;
    }

    // The ticket is THIS command's identity; its outcome is reported for it and no other.
    match control.try_command_with_reason(cmd, reason) {
        Ok(ticket) => match control.await_outcome(ticket, ACK_WAIT) {
            Some(CommandOutcome::Accepted { coid }) => {
                let named = if coid.is_empty() { String::new() } else { format!(" (coid {coid})") };
                println!(
                    "accepted by the node{named} — run `orders`/`positions` to observe the result; \
                     the node's ControlLimits + RiskGate are the enforcing gate"
                );
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
            None => println!(
                "sent, but the node has not answered within {}s — the outcome is NOT yet known; \
                 check `orders`/`positions` before retrying",
                ACK_WAIT.as_secs()
            ),
        },
        Err(e) => println!("not sent: {e:?}"),
    }
}

/// Prompt `confirm? [y/N]` and return true only on an explicit `y`/`yes`. Ctrl-C / Ctrl-D / anything
/// else is a "no" (the safe default for an order-write).
fn confirm(rl: &mut DefaultEditor) -> bool {
    match rl.readline("confirm? [y/N] ") {
        Ok(answer) => matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes"),
        Err(_) => false,
    }
}

/// Basename of the persisted REPL history, inside the settings directory's `state/`.
///
/// PROGRAM-written, so it belongs under `state/` rather than beside the TOMLs a human edits: a
/// program rewriting a file somebody is editing loses that person's work.
const HISTORY_FILE: &str = "trade_history";

/// `<project>/settings/state/trade_history` (best-effort; `None` when no project sits above the
/// working directory, in which case history is simply not persisted).
///
/// PURE — the settings directory arrives as a parameter, resolved once by `crate::run` from the
/// same `std::env::vars()` sweep it already performs for the policy ceiling.
fn history_file(settings_dir: Option<&std::path::Path>) -> Option<std::path::PathBuf> {
    Some(settings_dir?.join(vike_secrets::STATE_DIR).join(HISTORY_FILE))
}

// ---- the verb grammar (PURE — no network, fully testable) ------------------------------------
//
// The [`Verb`] enum itself (and its `is_write` / `to_wire_command`) lives in [`crate::cmd::verbs`]
// — the ONE args→WireCommand construction site shared with the `mcp` write tools — and is
// re-exported above. Only the REPL line GRAMMAR stays here.

/// The `--reason` tail marker: everything after it on the line is the operator rationale.
const REASON_FLAG: &str = "--reason";

/// Split a REPL line into `(command, reason)` at the first `--reason` TOKEN — run BEFORE
/// [`parse_line`], so the verb grammar never has to know the rationale exists (and a rationale
/// containing a word like `--qty` can never be mistaken for a flag).
///
/// The rationale is taken VERBATIM to end of line, so a multi-word "why" needs no quoting; it is
/// trimmed, and ONE optional layer of surrounding `"`/`'` quotes is removed for the habitual typist.
/// `--reason` with nothing after it (or only whitespace) yields `None` — an empty rationale is no
/// rationale. `--reason` matches only as a whole whitespace-delimited token, so a symbol or coid that
/// merely CONTAINS the text is not a split point. No `--reason` at all ⇒ `(line, None)`, byte-for-byte
/// what the parser saw before this existed. PURE.
pub fn split_reason(line: &str) -> (&str, Option<String>) {
    let mut cursor = 0usize;
    let at = loop {
        let Some(hit) = line[cursor..].find(REASON_FLAG) else { return (line, None) };
        let at = cursor + hit;
        let after = &line[at + REASON_FLAG.len()..];
        let is_token = (at == 0 || line[..at].ends_with(char::is_whitespace))
            && (after.is_empty() || after.starts_with(char::is_whitespace));
        if is_token {
            break at;
        }
        cursor = at + REASON_FLAG.len();
    };
    let command = line[..at].trim_end();
    let reason = unquote(line[at + REASON_FLAG.len()..].trim());
    if reason.is_empty() {
        (command, None)
    } else {
        (command, Some(reason.to_string()))
    }
}

/// Strip ONE layer of matching surrounding `"` or `'` quotes, if present. Byte indexing is safe: both
/// quote characters are single-byte, so slicing just inside them lands on char boundaries.
fn unquote(s: &str) -> &str {
    let b = s.as_bytes();
    let quoted = b.len() >= 2 && b[0] == b[b.len() - 1] && (b[0] == b'"' || b[0] == b'\'');
    if quoted {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// Parse one REPL line into a [`Verb`]. Terse, whitespace-tokenized. Malformed input is a clean
/// `Err(String)`. PURE — no network, no I/O — so the whole grammar is unit-testable.
///
/// The line handed here has already had any `--reason` tail split off by [`split_reason`].
pub fn parse_line(line: &str) -> Result<Verb, String> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let (verb, rest) = tokens.split_first().ok_or("empty line")?;
    match verb.to_ascii_lowercase().as_str() {
        "submit" | "buy" | "sell" => parse_submit(verb, rest),
        "cancel" => {
            let coid = rest.first().ok_or("usage: cancel <coid>")?;
            if rest.len() > 1 {
                return Err("usage: cancel <coid>".to_string());
            }
            Ok(Verb::Cancel((*coid).to_string()))
        }
        "modify" => parse_modify(rest),
        "flatten" => {
            if rest.len() != 2 {
                return Err("usage: flatten <venue> <symbol>".to_string());
            }
            Ok(Verb::Flatten { venue: rest[0].to_string(), symbol: rest[1].to_string() })
        }
        "market-exit" | "panic" => {
            if rest.len() > 1 {
                return Err("usage: market-exit [venue]".to_string());
            }
            Ok(Verb::MarketExit { venue: rest.first().map(|s| s.to_string()) })
        }
        "mass-cancel" => {
            if rest.len() > 2 {
                return Err("usage: mass-cancel [venue] [symbol]".to_string());
            }
            Ok(Verb::MassCancel {
                venue: rest.first().map(|s| s.to_string()),
                symbol: rest.get(1).map(|s| s.to_string()),
            })
        }
        "state" => match rest.first() {
            None => Ok(Verb::ShowState),
            Some(s) => Ok(Verb::SetState(parse_state(s)?)),
        },
        "orders" => {
            if rest.len() > 1 {
                return Err("usage: orders [symbol]".to_string());
            }
            Ok(Verb::Orders(rest.first().map(|s| s.to_string())))
        }
        "positions" | "pos" => {
            if rest.len() > 1 {
                return Err("usage: positions [venue]".to_string());
            }
            Ok(Verb::Positions(rest.first().map(|s| s.to_string())))
        }
        "equity" | "balance" => Ok(Verb::Equity),
        "snapshot" | "snap" => Ok(Verb::Snapshot),
        "recent" => match rest.first() {
            None => Ok(Verb::Recent(None)),
            Some(n) => {
                let n: usize = n.parse().map_err(|_| format!("recent: not a count: {n:?}"))?;
                Ok(Verb::Recent(Some(n)))
            }
        },
        "help" | "?" => Ok(Verb::Help),
        "quit" | "exit" | "q" => Ok(Verb::Quit),
        other => Err(format!("unknown command: {other:?}")),
    }
}

/// `submit <venue> <symbol> <buy|sell> <qty> [@<price>|@market] [--reduce-only] [--coid <id>]`. The
/// leading verb may also be `buy`/`sell` as sugar (then side is implied and the `<buy|sell>` token
/// is omitted).
///
/// `--coid <id>` PINS the client-order-id instead of letting the session mint one. It exists
/// because the sibling MCP tool already takes an optional `client_order_id` and the two surfaces
/// share one vocabulary by design — plus the two cases a human actually hits: adopting an id
/// somebody else (a script, a runbook, the venue-side reconciliation sheet) already wrote down, and
/// scripting `--yes` where the caller needs to know the id BEFORE the command runs.
///
/// It is validated with `vike_model::is_valid_crypto_coid` — the same `^[A-Za-z0-9]{1,32}$` rule
/// the minter's own output is asserted against, and the strictest common denominator across venues.
/// Accepting something looser here would only move the failure to the venue edge, minutes later and
/// far from the typo. Left EMPTY (`--coid ""`) it is rejected outright rather than silently falling
/// back to a mint: an operator who names an id and gets a different one has lost the handle they
/// asked for.
fn parse_submit(verb: &str, rest: &[&str]) -> Result<Verb, String> {
    const USAGE: &str = "usage: submit <venue> <symbol> <buy|sell> <qty> [@<price>|@market] \
                         [--reduce-only] [--coid <id>]";
    let mut reduce_only = false;
    let mut positional: Vec<&str> = Vec::new();
    let mut price_tok: Option<&str> = None;
    let mut coid: Option<&str> = None;
    let mut expect_coid = false;
    for tok in rest {
        if expect_coid {
            expect_coid = false;
            coid = Some(tok);
            continue;
        }
        if *tok == "--reduce-only" {
            reduce_only = true;
        } else if let Some(v) = tok.strip_prefix("--coid=") {
            coid = Some(v);
        } else if *tok == "--coid" {
            expect_coid = true;
        } else if let Some(p) = tok.strip_prefix('@') {
            if price_tok.is_some() {
                return Err("submit: more than one @price token".to_string());
            }
            price_tok = Some(p);
        } else {
            positional.push(tok);
        }
    }
    if expect_coid {
        return Err("submit: --coid needs a value".to_string());
    }
    let client_order_id = match coid {
        None => String::new(), // the session mints one at preview time
        Some(id) if vike_model::is_valid_crypto_coid(id) => id.to_string(),
        Some(id) => {
            return Err(format!(
                "submit: --coid {id:?} is not a usable client_order_id — it must be {} (the \
                 strictest charset every venue accepts; `help` states it too)",
                verbs::COID_CHARSET
            ))
        }
    };

    // `buy`/`sell` sugar: `buy <venue> <symbol> <qty>` (3 positionals; side is the verb). Full form:
    // `submit <venue> <symbol> <buy|sell> <qty>` (4 positionals; side is the 3rd). The qty is the
    // LAST positional in BOTH shapes.
    let (venue, symbol, side) = match verb.to_ascii_lowercase().as_str() {
        "buy" | "sell" => {
            if positional.len() != 3 {
                return Err(format!("usage: {verb} <venue> <symbol> <qty> [@<price>|@market]"));
            }
            (positional[0], positional[1], side_from_word(verb)?)
        }
        _ => {
            if positional.len() != 4 {
                return Err(USAGE.to_string());
            }
            (positional[0], positional[1], side_from_word(positional[2])?)
        }
    };
    let qty_tok = positional.last().expect("checked non-empty above");
    let qty: f64 = qty_tok.parse().map_err(|_| format!("submit: not a qty: {qty_tok:?}"))?;
    if qty.is_nan() || qty <= 0.0 {
        return Err("submit: qty must be > 0".to_string());
    }

    // Resolve the order type from the @price token: absent or `@market` = market; `@<num>` = limit.
    let (order_type, price) = match price_tok {
        None | Some("market") => ("market", None),
        Some(p) => {
            let px: f64 = p.parse().map_err(|_| format!("submit: not a price: @{p}"))?;
            ("limit", Some(px))
        }
    };

    Ok(Verb::Submit(WireOrderRequest {
        // EMPTY unless `--coid` pinned one. `run_write` fills it from the session's generator
        // before the preview prints — deliberately NOT here, so this parser stays PURE and every
        // grammar test below is deterministic. ⚠ It used to be left empty all the way onto the
        // wire "for the runtime to mint", which the node refuses outright.
        client_order_id,
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        side,
        qty,
        order_type: order_type.to_string(),
        price,
        trigger_price: None,
        reduce_only,
    }))
}

/// `modify <coid> [--qty Q] [--price P]` — at least one of `--qty`/`--price` required.
fn parse_modify(rest: &[&str]) -> Result<Verb, String> {
    const USAGE: &str = "usage: modify <coid> [--qty Q] [--price P]";
    let coid = rest.first().ok_or(USAGE)?;
    let mut new_qty: Option<f64> = None;
    let mut new_price: Option<f64> = None;
    let mut i = 1;
    while i < rest.len() {
        match rest[i] {
            "--qty" => {
                let v = rest.get(i + 1).ok_or("modify: --qty needs a value")?;
                new_qty = Some(v.parse().map_err(|_| format!("modify: not a qty: {v:?}"))?);
                i += 2;
            }
            "--price" => {
                let v = rest.get(i + 1).ok_or("modify: --price needs a value")?;
                new_price = Some(v.parse().map_err(|_| format!("modify: not a price: {v:?}"))?);
                i += 2;
            }
            other => return Err(format!("modify: unexpected token {other:?}\n{USAGE}")),
        }
    }
    if new_qty.is_none() && new_price.is_none() {
        return Err("modify: nothing to change — pass --qty and/or --price".to_string());
    }
    Ok(Verb::Modify { client_order_id: (*coid).to_string(), new_qty, new_price })
}

/// `buy`/`sell` (or `b`/`s`) → +1 / -1.
fn side_from_word(word: &str) -> Result<i32, String> {
    match word.to_ascii_lowercase().as_str() {
        "buy" | "b" | "long" => Ok(1),
        "sell" | "s" | "short" => Ok(-1),
        other => Err(format!("side must be buy|sell, got {other:?}")),
    }
}

/// `active`/`reducing`/`halted` → the wire trading state.
fn parse_state(word: &str) -> Result<WireTradingState, String> {
    match word.to_ascii_lowercase().as_str() {
        "active" => Ok(WireTradingState::Active),
        "reducing" => Ok(WireTradingState::Reducing),
        "halted" => Ok(WireTradingState::Halted),
        other => Err(format!("state must be active|reducing|halted, got {other:?}")),
    }
}

// ---- human rendering (of the pure command / of the snapshot) ---------------------------------

/// A one-line human description of a resolved [`WireCommand`] for the preview.
///
/// ⚠ `qty` and `price` go through [`verbs::fmt_num`], the SAME renderer the guardrail line under
/// this one uses. They must: the two lines print the same quantities, and a preview reading `qty=3`
/// above a guardrail reading `qty=3.0000000000000004` would leave the operator deciding which of the
/// tool's own two lines to believe.
fn describe_command(cmd: &WireCommand) -> String {
    match cmd {
        // `coid=` is part of the preview because it is the HANDLE: it is what the operator types
        // back at `cancel <coid>` / `modify <coid>`, and (since the mint happens before this runs)
        // it is exactly the id that goes on the wire.
        WireCommand::Submit(o) => format!(
            "SUBMIT {} {} {} qty={} {}{} coid={}",
            side_word(o.side),
            o.venue,
            o.symbol,
            verbs::fmt_num(o.qty),
            match o.price {
                Some(p) => format!("{} @ {}", o.order_type, verbs::fmt_num(p)),
                None => o.order_type.clone(),
            },
            if o.reduce_only { " [reduce-only]" } else { "" },
            o.client_order_id,
        ),
        WireCommand::Cancel(coid) => format!("CANCEL {coid}"),
        WireCommand::Modify { client_order_id, new_qty, new_price } => format!(
            "MODIFY {client_order_id} qty={} price={}",
            new_qty.map(verbs::fmt_num).unwrap_or_else(|| "-".to_string()),
            new_price.map(verbs::fmt_num).unwrap_or_else(|| "-".to_string()),
        ),
        WireCommand::MassCancel { venue, symbol } => {
            format!("MASS-CANCEL venue={} symbol={}", opt_str(venue), opt_str(symbol))
        }
        WireCommand::Flatten { venue, symbol } => format!("FLATTEN {venue} {symbol}"),
        WireCommand::MarketExit { venue } => format!("MARKET-EXIT venue={}", opt_str(venue)),
        WireCommand::SetTradingState(s) => format!("SET-STATE {s:?}"),
        // No `trade` REPL verb PRODUCES this yet (split-plane B4 wired the wire form + daemon
        // lowering; a CLI spelling is future work), but the preview renderer must stay total: show
        // the target mount and the raw params JSON, never panic on a verb another caller minted.
        WireCommand::UpdateParams { venue, symbol, interval, params } => {
            format!("UPDATE-PARAMS {venue} {symbol} {interval} {params}")
        }
        // Same posture for the B5 mount verbs: no REPL spelling yet, but the renderer stays total.
        WireCommand::MountStrategy {
            venue,
            symbol,
            interval,
            controller_id,
            name,
            rhai,
            params,
        } => {
            format!(
                "MOUNT-STRATEGY {venue} {symbol} {interval} id={} source={} {params}",
                controller_id.as_deref().unwrap_or("-"),
                name.as_deref().or(rhai.as_deref()).unwrap_or("-"),
            )
        }
        WireCommand::UnmountStrategy { controller_id } => {
            format!("UNMOUNT-STRATEGY {controller_id}")
        }
        // Same posture for the REQ-7 settings write: no REPL spelling yet (the GUI's
        // Connections section is the shipped surface), but the renderer stays total — show the
        // assignment and whether a typed confirm rides along, never its value.
        WireCommand::SetSetting { file, key, value, confirm } => {
            format!(
                "SET-SETTING {file} {key} = {value}{}",
                if confirm.is_some() { " [confirmed]" } else { "" }
            )
        }
    }
}

fn side_word(side: i32) -> &'static str {
    if side >= 0 {
        "buy"
    } else {
        "sell"
    }
}

fn opt_num(v: &Option<f64>) -> String {
    v.map(|n| n.to_string()).unwrap_or_else(|| "-".to_string())
}

fn opt_str(v: &Option<String>) -> String {
    v.clone().unwrap_or_else(|| "*".to_string())
}

/// A `[seq N]` staleness stamp for a snapshot render (the disconnected note is printed separately by
/// [`with_snapshot`] so it shows even when a render function does not include the stamp).
fn stamp(snap: &WireSnapshot) -> String {
    format!("[seq {}]", snap.seq)
}

fn print_orders(snap: &WireSnapshot, symbol: Option<&str>) {
    println!("{}", orders_table(snap, symbol));
}

/// Render the orders view. Returns the text rather than printing it so the two column rules below
/// are testable — the difference between a coid an operator can type back at `cancel` and one they
/// cannot is not a cosmetic property.
fn orders_table(snap: &WireSnapshot, symbol: Option<&str>) -> String {
    let mut out = format!("{} orders", stamp(snap));
    let rows: Vec<_> = snap
        .orders
        .iter()
        .filter(|o| symbol.is_none_or(|s| o.symbol.eq_ignore_ascii_case(s)))
        .collect();
    if rows.is_empty() {
        out.push_str("\n  (none)");
        return out;
    }
    // The coid column is NEVER truncated. It is the HANDLE — what the operator reads off this table
    // and types back at `cancel <coid>` / `modify <coid>` — so a shortened one does not make the
    // table ugly, it makes it unusable. It was a fixed 20 while `submit --coid` accepts up to 32
    // (`vike_model::is_valid_crypto_coid`), and a node may report an id this side never minted.
    let wc = width_of("coid", rows.iter().map(|o| o.client_order_id.as_str()), None);
    // The symbol column IS capped — no width fits a Polymarket token id (a decimal uint256, ~77
    // digits) — but it is truncated in the MIDDLE. Head-only truncation rendered `DUMMYTOKEN000…`,
    // which identifies no order at all: the token ids of one up/down family share a long prefix and
    // differ at the TAIL, so the end is the half that carries the identity.
    let ws = width_of("symbol", rows.iter().map(|o| o.symbol.as_str()), Some(SYMBOL_MAX));
    out.push_str(&format!(
        "\n  {:<wc$} {:<10} {:<ws$} {:<5} {:>12} {:<10} {:>12} {:<12} {:>12}",
        "coid", "venue", "symbol", "side", "qty", "type", "price", "status", "filled"
    ));
    for o in rows {
        out.push_str(&format!(
            "\n  {:<wc$} {:<10} {:<ws$} {:<5} {:>12} {:<10} {:>12} {:<12} {:>12}",
            o.client_order_id,
            trunc(&o.venue, 10),
            trunc_mid(&o.symbol, ws),
            side_word(o.side),
            o.qty,
            trunc(&o.order_type, 10),
            opt_num(&o.price),
            trunc(&o.status, 12),
            o.filled_qty,
        ));
    }
    out
}

fn print_positions(snap: &WireSnapshot, venue: Option<&str>) {
    println!("{} positions", stamp(snap));
    let rows: Vec<_> = snap
        .positions
        .iter()
        .filter(|p| venue.is_none_or(|v| p.venue.eq_ignore_ascii_case(v)))
        .collect();
    if rows.is_empty() {
        println!("  (none)");
        return;
    }
    // Same symbol rule as the orders table above, and for the same reason — a position in a
    // Polymarket token is as unidentifiable from a 14-char prefix as an order in one.
    let ws = width_of("symbol", rows.iter().map(|p| p.symbol.as_str()), Some(SYMBOL_MAX));
    println!(
        "  {:<10} {:<ws$} {:<6} {:>12} {:>12} {:>12} {:>8} {:>12}",
        "venue", "symbol", "side", "size", "avg_px", "unreal", "lev", "liq"
    );
    for p in rows {
        println!(
            "  {:<10} {:<ws$} {:<6} {:>12} {:>12} {:>12} {:>8} {:>12}",
            trunc(&p.venue, 10),
            trunc_mid(&p.symbol, ws),
            trunc(&p.position_side, 6),
            p.size,
            p.avg_px,
            p.unrealized,
            p.leverage,
            p.liq_price,
        );
    }
}

fn print_equity(snap: &WireSnapshot) {
    println!("{} equity", stamp(snap));
    println!("  balance (primary): {}", snap.balance);
    println!("  equity_total:      {}", snap.equity_total);
    if !snap.venues.is_empty() {
        println!(
            "  {:<10} {:>12} {:>12} {:>12} {:>12} {:>12}",
            "venue", "balance", "realized", "equity", "unreal", "free_bp"
        );
        for v in &snap.venues {
            println!(
                "  {:<10} {:>12} {:>12} {:>12} {:>12} {:>12}",
                trunc(&v.venue, 10),
                v.balance,
                v.realized_pnl,
                v.equity,
                v.unrealized,
                v.free_bp,
            );
        }
    }
}

fn print_recent(snap: &WireSnapshot, n: Option<usize>) {
    println!("{} recent events", stamp(snap));
    let events = &snap.recent_events;
    if events.is_empty() {
        println!("  (none)");
        return;
    }
    let take = n.unwrap_or(events.len()).min(events.len());
    for e in &events[events.len() - take..] {
        println!("  {e}");
    }
}

fn print_state(snap: &WireSnapshot) {
    println!("{} trading state: {:?}", stamp(snap), snap.trading_state);
    if let Some(fault) = &snap.fault {
        println!("  FAULT (core halted safe-state): {fault}");
    }
}

fn print_snapshot(snap: &WireSnapshot) {
    print_state(snap);
    print_equity(snap);
    print_orders(snap, None);
    print_positions(snap, None);
    print_recent(snap, Some(5));
}

/// The widest a SYMBOL cell may grow before [`trunc_mid`] shortens it. A cap is unavoidable — a
/// Polymarket token id is a decimal uint256, ~77 digits, and one of them would push every column
/// after it off an 80-column terminal — but it is generous enough that no ordinary instrument
/// (`BTCUSDT`, `EUR/USD`, `BTC-30AUG26-120000-C`) is touched at all.
const SYMBOL_MAX: usize = 24;

/// Width for a variable-width column: wide enough for the header and every cell, bounded by `cap`
/// when one is given. `None` means NEVER truncate — the shape the coid column needs, because that
/// column is an input the operator retypes, not a label.
fn width_of<'a>(header: &str, cells: impl Iterator<Item = &'a str>, cap: Option<usize>) -> usize {
    let widest = cells.map(|c| c.chars().count()).max().unwrap_or(0).max(header.chars().count());
    match cap {
        Some(c) => widest.min(c),
        None => widest,
    }
}

/// Truncate a string to `max` chars for column display (keeps the aligned tables from smearing).
fn trunc(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Truncate keeping BOTH ends — `1234567…8901` — for cells that are IDENTIFIERS rather than labels.
///
/// [`trunc`]'s head-only form is right for a venue or a status, where the first characters name the
/// thing. It is wrong for a symbol: Polymarket token ids are long decimal strings, the tokens of one
/// up/down family share a long prefix, and they differ at the TAIL — so a head-only cell rendered
/// `DUMMYTOKEN000…` for every order in the family and identified none of them.
fn trunc_mid(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    if max <= 1 {
        // Nothing but the marker fits — and at 0 not even that.
        return "…".chars().take(max).collect();
    }
    let keep = max - 1; // one column for the ellipsis
    let head = keep.div_ceil(2);
    let tail = keep - head;
    let mut out: String = s.chars().take(head).collect();
    out.push('…');
    out.extend(s.chars().skip(n - tail));
    out
}

/// Print the terse verb reference (the `help` verb + the `--help` startup path).
fn print_verb_help() {
    println!("commands:");
    println!("  WRITE (preview + confirm):");
    println!(
        "    submit <venue> <symbol> <buy|sell> <qty> [@<price>|@market] [--reduce-only] \
         [--coid <id>]"
    );
    println!(
        "    buy|sell <venue> <symbol> <qty> [@<price>|@market] [--reduce-only] [--coid <id>]"
    );
    println!(
        "      (a client_order_id is MINTED per order and shown in the preview; --coid pins one)"
    );
    // STATE THE CHARSET HERE. `submit --coid` refuses a bad one with a good message, but a rejection
    // is the wrong place to learn a constraint the help screen could have stated — and `cancel`,
    // which takes the same field, only WARNS (it must; see `verbs::coid_charset_warning`), so the
    // help is the one surface that can state the rule once for every verb that takes a coid.
    println!("      <id> is {}", verbs::COID_CHARSET);
    println!("    cancel <coid>                (fire-and-forget: the node cannot confirm a match)");
    println!("    modify <coid> [--qty Q] [--price P]");
    println!("    flatten <venue> <symbol>");
    println!("    mass-cancel [venue] [symbol]");
    println!("    market-exit [venue]          (panic: cancel all + flatten all)");
    println!("    state <active|reducing|halted>");
    println!("  any WRITE line may end with:");
    println!("    --reason <text to end of line>   recorded in the node's audit trail");
    println!("  READ:");
    println!("    orders [symbol]");
    println!("    positions [venue]");
    println!("    equity");
    println!("    recent [N]");
    println!("    state                        (show current)");
    println!("    snapshot");
    println!("  meta:  help    quit|exit  (or Ctrl-D)");
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- why writes are disabled: three causes, three messages -------------------------------------

    /// **The reported defect.** With `tradehub_control` off at the node and a VALID control key in
    /// the credential store, a write printed
    /// `writes disabled (OBSERVE-ONLY): no VIKE_TRADEHUB_CONTROL_KEY — nothing was sent`. The key was
    /// present and fine; the node had refused the SCOPE, and had said so — once, at connect time,
    /// before scrolling away. The operator was sent hunting for a key that was never the problem.
    #[test]
    fn a_scope_refusal_never_reports_itself_as_a_missing_key() {
        let denied = std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "tradehub control auth denied: control disabled on this node",
        );
        let cause = NoControl::from_connect_error(&denied);
        assert_eq!(
            cause,
            NoControl::Refused(
                "tradehub control auth denied: control disabled on this node".into()
            )
        );

        let line = cause.refusal_line();
        assert!(
            !line.contains(&format!("no {}", nodekeys::CONTROL_KEY_ENV)),
            "the message must not blame the key when the key authenticated: {line}"
        );
        assert!(
            line.contains("control disabled on this node"),
            "it must carry the node's own \
             reason, which is the actionable half: {line}"
        );
        assert!(line.contains("tradehub_control"), "…and name the flag that fixes it: {line}");
        assert!(line.contains("nothing was sent"), "every cause must state this: {line}");
    }

    /// The case the old message WAS right about keeps saying so — the fix must not make the honest
    /// path vaguer to make the dishonest one honest.
    #[test]
    fn an_absent_key_still_says_the_key_is_absent() {
        let line = NoControl::NoKey.refusal_line();
        assert!(line.contains(nodekeys::CONTROL_KEY_ENV), "{line}");
        assert!(line.contains("credential store"), "it must name BOTH places we looked: {line}");
        assert!(line.contains("nothing was sent"), "{line}");
    }

    /// A transport failure is neither of the above: nothing is known about the key or the node's
    /// policy, so the message claims neither. Classified by ERROR KIND, not by message text — the
    /// node is free to reword its reason, and `PermissionDenied` is the one kind `node_handshake`
    /// maps `Response::AuthDenied` to.
    #[test]
    fn an_unreachable_node_blames_neither_the_key_nor_the_node_policy() {
        let refused =
            std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "Connection refused");
        let cause = NoControl::from_connect_error(&refused);
        assert!(matches!(cause, NoControl::Unreachable(_)), "{cause:?}");

        let line = cause.refusal_line();
        assert!(!line.contains(&format!("no {}", nodekeys::CONTROL_KEY_ENV)), "{line}");
        assert!(
            !line.contains("tradehub_control"),
            "nothing is known about the node's flag: {line}"
        );
        assert!(line.contains("Connection refused") && line.contains("nothing was sent"), "{line}");
    }

    /// The three are genuinely different ACTIONS — put a key in the store / turn the node's flag on /
    /// fix the network — so no two may print the same line.
    #[test]
    fn the_three_causes_print_three_different_lines() {
        let lines = [
            NoControl::NoKey.refusal_line(),
            NoControl::Refused("control disabled on this node".into()).refusal_line(),
            NoControl::Unreachable("Connection refused".into()).refusal_line(),
        ];
        for (i, a) in lines.iter().enumerate() {
            for b in lines.iter().skip(i + 1) {
                assert_ne!(a, b, "two distinct causes print the same message");
            }
        }
    }

    fn submit(v: Verb) -> WireOrderRequest {
        match v {
            Verb::Submit(o) => o,
            other => panic!("expected Submit, got {other:?}"),
        }
    }

    #[test]
    fn parse_submit_limit_with_reduce_only() {
        let v = parse_line("submit binance BTCUSDT buy 0.5 @59000 --reduce-only").unwrap();
        assert!(v.is_write());
        let o = submit(v);
        assert_eq!(o.venue, "binance");
        assert_eq!(o.symbol, "BTCUSDT");
        assert_eq!(o.side, 1);
        assert_eq!(o.qty, 0.5);
        assert_eq!(o.order_type, "limit");
        assert_eq!(o.price, Some(59000.0));
        assert!(o.reduce_only);
        // The PARSER leaves it empty; `run_write` mints before the preview (see the two tests
        // below and `verbs::fill_client_order_id`). Keeping the parser pure is what makes every
        // grammar test here deterministic.
        assert!(o.client_order_id.is_empty());
    }

    /// **THE defect, at this surface.** A parsed `submit` carries no coid, and the node REFUSES a
    /// remote submit with an empty one — so what actually goes on the wire must be the MINTED
    /// command. This pins the exact composition `run_write` performs (mint, then describe), which
    /// is the step that was missing entirely.
    #[test]
    fn a_submit_reaches_the_wire_with_a_minted_venue_valid_coid() {
        let cmd = parse_line("submit sim BTCUSDT buy 1 @100").unwrap().to_wire_command().unwrap();
        let WireCommand::Submit(unminted) = &cmd else { panic!("expected Submit") };
        assert!(unminted.client_order_id.is_empty(), "the parser mints nothing");

        let mut coids = verbs::coid_minter();
        let sent = verbs::fill_client_order_id(cmd, &mut coids);
        let WireCommand::Submit(o) = &sent else { panic!("still a Submit") };
        assert!(
            vike_model::is_valid_crypto_coid(&o.client_order_id),
            "the node refuses an empty coid and the venue refuses a non-alphanumeric one: {:?}",
            o.client_order_id
        );
        // …and the operator can SEE it: the preview line carries the handle they type back.
        let preview = describe_command(&sent);
        assert!(
            preview.contains(&format!("coid={}", o.client_order_id)),
            "the preview must show the id that is sent: {preview}"
        );
    }

    /// `--coid` pins an id (both flag forms), and an unusable one is a clean parse error rather
    /// than a silent fallback to a minted id the caller did not ask for.
    #[test]
    fn coid_override_pins_the_id_and_rejects_an_unusable_one() {
        for line in
            ["submit sim BTCUSDT buy 1 @100 --coid abc123", "buy sim BTCUSDT 1 --coid=abc123"]
        {
            let o = submit(parse_line(line).unwrap());
            assert_eq!(o.client_order_id, "abc123", "{line}");
        }
        // A pinned id survives the mint untouched — that is what "pin" means.
        let cmd = parse_line("submit sim BTCUSDT buy 1 --coid abc123").unwrap().to_wire_command();
        let mut coids = verbs::coid_minter();
        let WireCommand::Submit(o) = verbs::fill_client_order_id(cmd.unwrap(), &mut coids) else {
            panic!("still a Submit")
        };
        assert_eq!(o.client_order_id, "abc123");

        assert!(parse_line("submit sim BTCUSDT buy 1 --coid").is_err()); // dangling value
        assert!(parse_line("submit sim BTCUSDT buy 1 --coid=").is_err()); // empty is not a pin
        assert!(parse_line("submit sim BTCUSDT buy 1 --coid has space").is_err()); // eats a positional
        assert!(parse_line("submit sim BTCUSDT buy 1 --coid c-1").is_err()); // '-' is not venue-safe
        assert!(parse_line(&format!("submit sim B buy 1 --coid {}", "x".repeat(33))).is_err());
    }

    #[test]
    fn parse_submit_market_default_and_explicit() {
        // No @price token → market.
        let o = submit(parse_line("submit sim ETHUSDT sell 2").unwrap());
        assert_eq!(o.side, -1);
        assert_eq!(o.order_type, "market");
        assert_eq!(o.price, None);
        assert!(!o.reduce_only);
        // Explicit @market → market.
        let o = submit(parse_line("submit sim ETHUSDT sell 2 @market").unwrap());
        assert_eq!(o.order_type, "market");
        assert_eq!(o.price, None);
    }

    #[test]
    fn parse_submit_buy_sell_sugar() {
        let o = submit(parse_line("buy binance BTCUSDT 1.0 @100").unwrap());
        assert_eq!(o.side, 1);
        assert_eq!(o.price, Some(100.0));
        let o = submit(parse_line("sell binance BTCUSDT 1.0").unwrap());
        assert_eq!(o.side, -1);
        assert_eq!(o.order_type, "market");
    }

    #[test]
    fn parse_submit_rejects_bad_input() {
        assert!(parse_line("submit binance BTCUSDT buy").is_err()); // missing qty
        assert!(parse_line("submit binance BTCUSDT sideways 1").is_err()); // bad side
        assert!(parse_line("submit binance BTCUSDT buy notaqty").is_err()); // bad qty
        assert!(parse_line("submit binance BTCUSDT buy 0").is_err()); // qty must be > 0
        assert!(parse_line("submit binance BTCUSDT buy -1").is_err()); // negative qty
        assert!(parse_line("submit binance BTCUSDT buy 1 @notaprice").is_err()); // bad price
        assert!(parse_line("submit only three tokens").is_err()); // too few positionals
    }

    #[test]
    fn parse_cancel() {
        let v = parse_line("cancel c-123").unwrap();
        assert!(v.is_write());
        assert_eq!(v, Verb::Cancel("c-123".into()));
        assert!(parse_line("cancel").is_err());
        assert!(parse_line("cancel a b").is_err());
    }

    #[test]
    fn parse_modify_variants() {
        assert_eq!(
            parse_line("modify c-1 --qty 2.0").unwrap(),
            Verb::Modify { client_order_id: "c-1".into(), new_qty: Some(2.0), new_price: None }
        );
        assert_eq!(
            parse_line("modify c-1 --price 100 --qty 3").unwrap(),
            Verb::Modify {
                client_order_id: "c-1".into(),
                new_qty: Some(3.0),
                new_price: Some(100.0)
            }
        );
        assert!(parse_line("modify c-1").is_err()); // nothing to change
        assert!(parse_line("modify").is_err()); // no coid
        assert!(parse_line("modify c-1 --qty").is_err()); // dangling value
        assert!(parse_line("modify c-1 --bogus 1").is_err()); // unknown flag
    }

    #[test]
    fn parse_flatten() {
        assert_eq!(
            parse_line("flatten binance BTCUSDT").unwrap(),
            Verb::Flatten { venue: "binance".into(), symbol: "BTCUSDT".into() }
        );
        assert!(parse_line("flatten binance").is_err());
        assert!(parse_line("flatten a b c").is_err());
    }

    #[test]
    fn parse_market_exit() {
        assert_eq!(parse_line("market-exit").unwrap(), Verb::MarketExit { venue: None });
        assert_eq!(
            parse_line("market-exit binance").unwrap(),
            Verb::MarketExit { venue: Some("binance".into()) }
        );
        assert_eq!(parse_line("panic").unwrap(), Verb::MarketExit { venue: None });
        assert!(parse_line("market-exit a b").is_err());
    }

    #[test]
    fn parse_mass_cancel() {
        assert_eq!(
            parse_line("mass-cancel").unwrap(),
            Verb::MassCancel { venue: None, symbol: None }
        );
        assert_eq!(
            parse_line("mass-cancel binance").unwrap(),
            Verb::MassCancel { venue: Some("binance".into()), symbol: None }
        );
        assert_eq!(
            parse_line("mass-cancel binance BTCUSDT").unwrap(),
            Verb::MassCancel { venue: Some("binance".into()), symbol: Some("BTCUSDT".into()) }
        );
        assert!(parse_line("mass-cancel a b c").is_err());
    }

    #[test]
    fn parse_state_read_vs_write() {
        // No arg = READ.
        let v = parse_line("state").unwrap();
        assert!(!v.is_write());
        assert_eq!(v, Verb::ShowState);
        // With arg = WRITE.
        for (word, want) in [
            ("active", WireTradingState::Active),
            ("reducing", WireTradingState::Reducing),
            ("halted", WireTradingState::Halted),
        ] {
            let v = parse_line(&format!("state {word}")).unwrap();
            assert!(v.is_write());
            assert_eq!(v, Verb::SetState(want));
        }
        assert!(parse_line("state bogus").is_err());
    }

    #[test]
    fn parse_read_verbs_are_not_writes() {
        for line in [
            "orders",
            "orders BTCUSDT",
            "positions",
            "positions binance",
            "equity",
            "balance",
            "snapshot",
            "snap",
            "recent",
            "recent 10",
        ] {
            let v = parse_line(line).unwrap();
            assert!(!v.is_write(), "{line:?} must be a read");
            assert!(v.to_wire_command().is_none(), "{line:?} maps to no wire command");
        }
        assert_eq!(parse_line("orders BTCUSDT").unwrap(), Verb::Orders(Some("BTCUSDT".into())));
        assert_eq!(
            parse_line("positions binance").unwrap(),
            Verb::Positions(Some("binance".into()))
        );
        assert_eq!(parse_line("recent 10").unwrap(), Verb::Recent(Some(10)));
        assert_eq!(parse_line("recent").unwrap(), Verb::Recent(None));
        assert!(parse_line("recent notanum").is_err());
        assert!(parse_line("orders a b").is_err());
    }

    #[test]
    fn parse_meta_and_unknown() {
        assert_eq!(parse_line("help").unwrap(), Verb::Help);
        assert_eq!(parse_line("quit").unwrap(), Verb::Quit);
        assert_eq!(parse_line("exit").unwrap(), Verb::Quit);
        assert!(parse_line("").is_err());
        assert!(parse_line("   ").is_err());
        assert!(parse_line("frobnicate").is_err());
    }

    /// Each WRITE verb maps to the correct [`WireCommand`]; each READ verb maps to `None`.
    #[test]
    fn verb_to_wire_command_mapping() {
        // Submit
        let cmd = parse_line("submit sim BTCUSDT buy 1 @100").unwrap().to_wire_command().unwrap();
        match cmd {
            WireCommand::Submit(o) => {
                assert_eq!(o.symbol, "BTCUSDT");
                assert_eq!(o.side, 1);
                assert_eq!(o.qty, 1.0);
                assert_eq!(o.price, Some(100.0));
            }
            other => panic!("expected Submit, got {other:?}"),
        }
        assert_eq!(
            parse_line("cancel c-9").unwrap().to_wire_command().unwrap(),
            WireCommand::Cancel("c-9".into())
        );
        assert_eq!(
            parse_line("modify c-9 --qty 5").unwrap().to_wire_command().unwrap(),
            WireCommand::Modify {
                client_order_id: "c-9".into(),
                new_qty: Some(5.0),
                new_price: None
            }
        );
        assert_eq!(
            parse_line("flatten sim BTCUSDT").unwrap().to_wire_command().unwrap(),
            WireCommand::Flatten { venue: "sim".into(), symbol: "BTCUSDT".into() }
        );
        assert_eq!(
            parse_line("mass-cancel sim").unwrap().to_wire_command().unwrap(),
            WireCommand::MassCancel { venue: Some("sim".into()), symbol: None }
        );
        assert_eq!(
            parse_line("market-exit").unwrap().to_wire_command().unwrap(),
            WireCommand::MarketExit { venue: None }
        );
        assert_eq!(
            parse_line("state halted").unwrap().to_wire_command().unwrap(),
            WireCommand::SetTradingState(WireTradingState::Halted)
        );
    }

    // ---- the `--reason` tail (node proto v4) --------------------------------------------------

    #[test]
    fn split_reason_takes_the_rest_of_the_line_verbatim() {
        let (cmd, why) = split_reason("flatten binance BTCUSDT --reason CPI print in 2 minutes");
        assert_eq!(cmd, "flatten binance BTCUSDT");
        assert_eq!(why.as_deref(), Some("CPI print in 2 minutes"));
        // …and the command half still parses exactly as it would have without the tail.
        assert_eq!(
            parse_line(cmd).unwrap(),
            Verb::Flatten { venue: "binance".into(), symbol: "BTCUSDT".into() }
        );
    }

    #[test]
    fn split_reason_absent_returns_the_whole_line_untouched() {
        let line = "submit binance BTCUSDT buy 0.5 @59000 --reduce-only";
        assert_eq!(split_reason(line), (line, None));
    }

    #[test]
    fn split_reason_strips_one_layer_of_quotes_and_treats_blank_as_none() {
        assert_eq!(split_reason("cancel c-1 --reason \"why not\"").1.as_deref(), Some("why not"));
        assert_eq!(split_reason("cancel c-1 --reason 'why not'").1.as_deref(), Some("why not"));
        // Only ONE layer, and only when it matches on both ends.
        assert_eq!(split_reason("cancel c-1 --reason \"a' ").1.as_deref(), Some("\"a'"));
        // Nothing (or only whitespace) after the flag is NO rationale.
        assert_eq!(split_reason("cancel c-1 --reason").1, None);
        assert_eq!(split_reason("cancel c-1 --reason    ").1, None);
        assert_eq!(split_reason("cancel c-1 --reason").0, "cancel c-1");
    }

    #[test]
    fn split_reason_only_matches_a_whole_token() {
        // A coid/symbol that merely CONTAINS the text is not a split point.
        for line in ["cancel my--reason-x", "cancel --reasonable", "cancel x--reason"] {
            assert_eq!(split_reason(line), (line, None), "{line:?} must not split");
        }
        // …but a real flag later in the same line still does.
        let (cmd, why) = split_reason("cancel --reasonable --reason it was a typo");
        assert_eq!(cmd, "cancel --reasonable");
        assert_eq!(why.as_deref(), Some("it was a typo"));
    }

    #[test]
    fn split_reason_leaves_the_verb_grammar_alone() {
        // A rationale containing flag-shaped words never reaches `parse_line` (the whole point of
        // splitting first): `modify` still sees only its own flags.
        let (cmd, why) = split_reason("modify c-1 --qty 2 --reason was --price too aggressive");
        assert_eq!(
            parse_line(cmd).unwrap(),
            Verb::Modify { client_order_id: "c-1".into(), new_qty: Some(2.0), new_price: None }
        );
        assert_eq!(why.as_deref(), Some("was --price too aggressive"));
    }

    #[test]
    fn config_parsing() {
        let c =
            parse_config(["--node", "the CI box:7979", "--yes"].into_iter().map(String::from)).unwrap();
        assert_eq!(c.node.as_deref(), Some("the CI box:7979"));
        assert!(c.yes);
        assert!(!c.help);
        // `--node=host:port` inline form.
        let c = parse_config(["--node=127.0.0.1:9".to_string()].into_iter()).unwrap();
        assert_eq!(c.node.as_deref(), Some("127.0.0.1:9"));
        // help short-circuits.
        let c = parse_config(["--help".to_string()].into_iter()).unwrap();
        assert!(c.help);
        // unknown arg errors.
        assert!(parse_config(["--bogus".to_string()].into_iter()).is_err());
        // dangling value errors.
        assert!(parse_config(["--node".to_string()].into_iter()).is_err());
        // a bare boolean rejects an inline value (the shared args glue; `--yes=1` used to be
        // silently accepted as true).
        assert!(parse_config(["--yes=1".to_string()].into_iter()).is_err());
    }

    // -- the orders table ----------------------------------------------------------------------

    /// A Polymarket-shaped order: a long decimal token id for a symbol, and a coid at the upper end
    /// of what `submit --coid` accepts.
    fn order(coid: &str, symbol: &str) -> vike_tradehub_client::wire::WireOrderView {
        vike_tradehub_client::wire::WireOrderView {
            client_order_id: coid.to_string(),
            venue: "polymarket".to_string(),
            symbol: symbol.to_string(),
            side: 1,
            qty: 10.0,
            order_type: "Limit".to_string(),
            price: Some(0.51),
            trigger_price: None,
            status: "Accepted".to_string(),
            venue_order_id: None,
            filled_qty: 0.0,
            avg_fill_px: 0.0,
        }
    }

    fn snap_with(orders: Vec<vike_tradehub_client::wire::WireOrderView>) -> WireSnapshot {
        WireSnapshot { orders, ..WireSnapshot::empty() }
    }

    /// The coid is the HANDLE — it is what the operator types back at `cancel <coid>` — so it must
    /// survive the table whole. The column was a fixed 20 while `submit --coid` accepts 32, so a
    /// pinned id came back `aaaaaaaaaaaaaaaaaaa…` and could not be retyped at all.
    #[test]
    fn the_coid_column_is_never_truncated() {
        let coid = "A".repeat(32);
        let table = orders_table(&snap_with(vec![order(&coid, "BTCUSDT")]), None);
        assert!(table.contains(&coid), "the coid must survive whole:\n{table}");
        assert!(!table.contains('…'), "nothing on this row is long enough to shorten:\n{table}");
    }

    /// The symbol IS capped (a Polymarket token id is a ~77-digit decimal and would push every later
    /// column off the terminal) but truncated in the MIDDLE: the tokens of one up/down family share
    /// a long prefix and differ at the TAIL, so a head-only cell named every order in the family
    /// identically — the reported `DUMMYTOKEN000…`.
    #[test]
    fn a_long_symbol_keeps_both_ends() {
        let a = format!("{}0001", "7".repeat(72));
        let b = format!("{}9999", "7".repeat(72));
        let table = orders_table(&snap_with(vec![order("c-1", &a), order("c-2", &b)]), None);

        assert!(table.contains("0001") && table.contains("9999"), "tails must survive:\n{table}");
        assert!(table.contains("7777"), "…and so must the head:\n{table}");
        // The two rows must be TELLABLE APART, which is the whole defect.
        let lines: Vec<&str> = table.lines().filter(|l| l.contains('…')).collect();
        assert_eq!(lines.len(), 2, "both rows truncate:\n{table}");
        assert_ne!(lines[0], lines[1], "two token ids rendered identically:\n{table}");
    }

    /// An ordinary instrument is untouched — the cap is generous enough that widening it cost the
    /// common case nothing.
    #[test]
    fn ordinary_symbols_are_not_truncated() {
        for symbol in ["BTCUSDT", "EUR/USD", "BTC-30AUG26-120000-C"] {
            let table = orders_table(&snap_with(vec![order("c-1", symbol)]), None);
            assert!(table.contains(symbol), "{symbol} was shortened:\n{table}");
        }
    }

    #[test]
    fn trunc_mid_keeps_both_ends_and_degrades_cleanly() {
        assert_eq!(trunc_mid("abcdef", 6), "abcdef", "a fitting string is untouched");
        assert_eq!(trunc_mid("abcdefghij", 5), "ab…ij");
        assert_eq!(trunc_mid("abcdefghij", 4), "ab…j", "an odd budget favours the head");
        assert_eq!(trunc_mid("abcdefghij", 2), "a…");
        assert_eq!(trunc_mid("abcdefghij", 1), "…");
        assert_eq!(trunc_mid("abcdefghij", 0), "");
        // Char-counted, not byte-counted — a multi-byte symbol must not panic on a slice boundary.
        assert_eq!(trunc_mid("ααααββββ", 5), "αα…ββ");
    }

    #[test]
    fn width_of_fits_the_header_and_honours_the_cap() {
        assert_eq!(width_of("coid", ["ab"].into_iter(), None), 4, "never narrower than the header");
        assert_eq!(width_of("coid", ["abcdefgh"].into_iter(), None), 8, "uncapped grows to fit");
        assert_eq!(width_of("coid", ["abcdefgh"].into_iter(), Some(5)), 5, "capped stops");
        assert_eq!(width_of("symbol", std::iter::empty(), Some(24)), 6, "no rows ⇒ the header");
    }

    /// The empty view still renders (and still carries its `[seq N]` stamp).
    #[test]
    fn an_empty_orders_table_says_none() {
        let table = orders_table(&snap_with(Vec::new()), None);
        assert!(table.contains("(none)"), "{table}");
        assert!(table.contains("[seq 0]"), "{table}");
    }
}
