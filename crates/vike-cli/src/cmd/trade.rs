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
//!   looked, including the node-key store's path.
//!
//! Both keys are resolved ONCE by the dispatcher and arrive as a [`NodeKeyring`]: the process
//! environment first, then `<project>/settings/node.env` — the same node-key store
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
//! # The strategy-LIFECYCLE half (`mount` / `unmount` / `set-setting`)
//!
//! Three verbs change what the node IS rather than what its books hold. They have a GRAMMAR of
//! their own — [`parse_lifecycle`] rather than [`parse_line`], because two of them take a
//! rest-of-line value whitespace tokenization would mangle — and they resolve to the SAME
//! [`crate::cmd::verbs`] vocabulary every order verb does. [`parse_repl_line`] is the one entry the
//! REPL loop calls and routes on the leading token, so a line still has exactly one grammar, and
//! both grammars now produce a [`Verb`]. They ride the SAME preview + `confirm? [y/N]` gate every
//! other write does.
//!
//! ⚠ This section used to say `Verb` was the ONE **order-write** vocabulary and that "none of these
//! three is an order", so each of them was parsed straight to a [`WireCommand`] here — while, in
//! parallel and under file ownership, the `mcp` server was building the same three from its own
//! JSON arguments in its own builder. Two construction sites for one wire command is exactly the
//! drift `verbs` exists to prevent, and both authors had written the lift down as the right move
//! the day the other surface grew them. It did; they are [`Verb`] variants now, and `verbs`' module
//! doc carries the argument for widening that type from order-write to node-write.
//!
//! - `mount` adds a strategy to the RUNNING core with no restart. Its source is the profile
//!   `[strategy]` vocabulary verbatim — a registry `--name` XOR a `--rhai` script path **on the
//!   NODE's filesystem** — and both-or-neither is a parse error naming the two spellings, because
//!   `WireCommand::MountStrategy`'s contract is an exclusive choice and a client that let the node
//!   discover that spends a round trip to say what the grammar already knew.
//! - `unmount` removes one mount BY ID. The node's core cancels that mount's attributed live orders
//!   and saves its durable state; **positions are NOT flattened** (`flatten` is the operator verb
//!   for that), and the help screen says so — an operator who reads "unmount" as "get me out" and
//!   is wrong about it is left holding an unattended position.
//! - `set-setting` writes ONE key into ONE of the node's four settings files, validated by the
//!   node's own loader before anything lands on disk.
//!
//! ⚠ **`set-setting` carries a SECOND confirmation, and `--yes` does not satisfy it.** A write whose
//! file is `policy.toml` is refused by the node unless the command's `confirm` field carries the
//! EXACT dotted key — policy holds the risk ceilings, so the operator RETYPES the key name. This
//! client therefore never pre-fills that field: [`parse_set_setting`] cannot produce one at all
//! (it is `None` by construction, and a test pins that), and the only thing that fills it is
//! [`typed_key_confirm`]'s interactive prompt, run AFTER the ordinary `y/N` gate and run even under
//! `--yes`. `--yes` skips the REPL's own convenience prompt; it may not answer the node's contract,
//! so a `policy.toml` write is deliberately the one line in this REPL a script cannot automate.
//!
//! ⚠ **That is a REPL behaviour and it is not the `mcp` server's**, deliberately: this surface has
//! an operator at a keyboard to ask, so it PROMPTS and compares what was typed; the MCP server has
//! an agent, so it REFUSES a policy write carrying no `policy_confirm` before it will even mint a
//! preview token, and performs no compare at all (a compare there would be a process checking a
//! string it could have written itself). The lift into [`crate::cmd::verbs`] shares the command
//! CONSTRUCTION between the two and nothing else — [`Verb::SetSetting`] carries the field, neither
//! parser may invent one, and what each surface then DOES about a policy file stays where it is
//! argued.
//!
//! ⚠ `set-setting` also leaves the shared fire-and-forget send path, and that is the point:
//! `RemoteControlHandle`'s worker maps an accepted settings write to an empty-coid `Accepted` and
//! DROPS the reply's `restart_required`, which is the one fact a settings write's caller actually
//! needs — did the node apply this now, or is it what the NEXT boot loads? So it goes through the
//! synchronous [`vike_tradehub_client::set_setting`] helper, which opens its own short-lived
//! control connection and returns that flag. See [`run_set_setting`].
//!
//! ⚠ **`UpdateParams` is deliberately NOT spelled here.** A one-field patch (`params gamma=0.0008`)
//! needs a structured READ of the mount's current params to patch against, and the node's read half
//! offers none — `WireMountRow::params` is a RENDERED string. Spelling the verb as "send a whole
//! params object" would let an operator who meant to change one knob silently reset the other
//! sixteen to a strategy's defaults. It is a separate design track, not an oversight.
//!
//! # What is / is not tested
//!
//! The PURE parts are unit-tested: [`parse_line`] / [`parse_lifecycle`] (every verb round-trips,
//! malformed input is rejected, `is_write` is correct) and the [`Verb::to_wire_command`] mapping.
//! The REPL loop, `rustyline`, and the live connection are thin glue over those pure parts (they
//! need a live node + a TTY), so they are not unit-tested — which is exactly why the two
//! confirmations above are structured so that the PARSER, not the prompt, is what proves the
//! typed-confirm field can never be pre-filled.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;
use serde_json::{Value, json};
use vike_model::client_order_id::ClientOrderIdGenerator;
use vike_tradehub_client::wire::{WireCommand, WireOrderRequest, WireSnapshot, WireTradingState};
use vike_tradehub_client::{
    CommandOutcome, ControlRejected, RemoteControlHandle, RemoteCoreHandle,
};

use crate::cmd::args::{self, Flags};
use crate::cmd::nodekeys::{self, NodeKeyring};
use crate::cmd::verbs;
use crate::cmd::verbs::Verb;
use crate::exit::Exit;

/// ⚠ The trailing note is not decoration. `--json` reached every other non-interactive surface, and
/// a gap with no explanation reads as an oversight somebody will "fix": a machine-readable REPL is
/// what `vike-cli mcp` already is — a whole stdio protocol with a tool schema, session state and a
/// mandatory preview — and a `--json` here would be a second, weaker one to keep in step with it.
/// The line exists so that the absence is a decision a reader can see rather than infer.
const USAGE: &str = "usage: vike-cli trade --node <host:port> [--yes]\n\
                     \n\
                     no --json: this is an interactive REPL for a human. The MACHINE surface over \
                     the same node\nis `vike-cli mcp`, which speaks stdio JSON-RPC and exposes the \
                     same verbs as tools.";

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
/// environment and, failing that, from the node-key store the daemon itself reads. Same
/// reasoning: the dispatcher owns both reads, this file takes the answer.
pub fn run(
    args: impl Iterator<Item = String>,
    policy_max_notional: Option<f64>,
    settings_dir: Option<std::path::PathBuf>,
    keys: &NodeKeyring,
) -> ExitCode {
    // ⚠ This verb does NOT go through `args::exit_for_parse_error` — it is a REPL with a `Config`
    // rather than an `Args`, and its help path prints the verb reference below instead of a usage
    // block. So the USAGE rung is spelled here, and `tests/exit_codes.rs` asserts it over the real
    // binary: a surface that owns its own copy of a shared decision is exactly the one that drifts.
    let cfg = match parse_config(args) {
        Ok(c) => c,
        Err(msg) => {
            eprintln!("vike-cli trade: {msg}\n{USAGE}");
            return Exit::Usage.into();
        }
    };
    if cfg.help {
        println!("{USAGE}\n");
        print_verb_help();
        return ExitCode::SUCCESS;
    }
    let Some(node) = cfg.node else {
        eprintln!("vike-cli trade: --node <host:port> is required\n{USAGE}");
        return Exit::Usage.into();
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
        // The CONNECT rung: a key resolved (the `has_any` gate above passed) and NEITHER half could
        // open a socket, so the node is unreachable rather than the command line wrong. Each half's
        // own reason is already on stderr above; this is the line that says the session cannot start.
        eprintln!("vike-cli trade: no usable connection to {node} — exiting");
        return Exit::Connect.into();
    }

    let mut session = Session {
        observe,
        control,
        control_denied,
        node,
        keys,
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
    /// No control key resolved at all: neither the process environment nor the node-key store had
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
                "writes disabled (OBSERVE-ONLY): no {} in the environment or the node-key store \
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
///
/// It borrows the [`NodeKeyring`] rather than copying a key out of it: [`run_set_setting`] needs the
/// control key a SECOND time (the synchronous settings helper opens its own short-lived connection),
/// and a session-lifetime `String` copy of a live node credential is a second place for one to
/// exist for no gain — the keyring is already alive for the whole call and already redacts itself in
/// `Debug`.
struct Session<'a> {
    observe: Option<RemoteCoreHandle>,
    control: Option<RemoteControlHandle>,
    /// Why [`Self::control`] is `None`, captured at connect time. `None` here means the control
    /// connection is UP. See [`NoControl`].
    control_denied: Option<NoControl>,
    /// `host:port` — the address both halves connected to, kept because the settings write reaches
    /// the node through its own connection rather than through [`Self::control`].
    node: String,
    /// The two node keys, resolved once by the dispatcher. See the struct doc for why it is a
    /// borrow.
    keys: &'a NodeKeyring,
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

/// The interactive loop. Reads a line, parses it ([`parse_repl_line`]), dispatches it. Ctrl-C
/// cancels the current line (continue), Ctrl-D quits. History persists under the settings
/// directory's `state/` (best-effort).
fn repl(session: &mut Session<'_>) -> Result<(), String> {
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
                match parse_repl_line(line) {
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

/// Run one parsed line against the live session. Reads print an aligned table from the latest
/// snapshot; writes preview + confirm + send. `reason` is the optional `--reason` rationale, which
/// only a WRITE carries (a read has no audit record to attach it to).
fn dispatch(session: &mut Session<'_>, rl: &mut DefaultEditor, verb: Verb, reason: Option<String>) {
    // ⚠ ONE write path, for orders and node lifecycle alike. The lifecycle three used to arrive
    // here already resolved to a `WireCommand` (their own construction site) and needed an arm of
    // their own above this branch; they are `Verb`s now, so `is_write` routes all ten.
    if verb.is_write() {
        match verb.to_wire_command() {
            Some(cmd) => run_write(session, rl, cmd, reason),
            // Unreachable through the grammar (`is_write` and `to_wire_command` agree by
            // construction in [`crate::cmd::verbs`]); kept because the two are separate methods and
            // a silent no-op on a WRITE would be the worst possible way for that to break.
            None => println!("error: not a write command"),
        }
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
fn with_snapshot(session: &Session<'_>, f: impl FnOnce(&WireSnapshot)) {
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

/// Preview + confirm + send one WRITE command. The node's server-side gate is the real enforcer.
/// `reason` is the optional operator rationale: shown in the preview and sent BESIDE the command for
/// the node's audit trail — it never becomes part of the order.
///
/// It takes the resolved [`WireCommand`] rather than a [`Verb`] because everything below this
/// point is about the wire form: the coid mint, the preview line, the guardrail and the settings
/// branch all read the command's VARIANT. Both grammars resolve through the one
/// [`Verb::to_wire_command`] before they get here.
fn run_write(
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
    // It also carries the node's typed-confirm contract, which no other verb has.
    if let WireCommand::SetSetting { file, key, value, .. } = &cmd {
        run_set_setting(session, rl, file, key, value, reason.as_deref());
        return;
    }

    let Some(control) = session.control.as_ref() else {
        // Unreachable: the gate above returned on `None`. Kept as a `let-else` rather than an
        // `expect` so a future edit that moves the gate degrades to a message instead of a panic in
        // an order surface.
        println!("[error] control connection is down — nothing was sent");
        return;
    };
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
}

/// Prompt `confirm? [y/N]` and return true only on an explicit `y`/`yes`. Ctrl-C / Ctrl-D / anything
/// else is a "no" (the safe default for an order-write).
fn confirm(rl: &mut DefaultEditor) -> bool {
    match rl.readline("confirm? [y/N] ") {
        Ok(answer) => matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes"),
        Err(_) => false,
    }
}

/// Send one `SetSetting` through the SYNCHRONOUS [`vike_tradehub_client::set_setting`] helper — its
/// own short-lived control connection — and report the node's verdict, including the
/// `restart_required` flag the fire-and-forget path cannot surface.
///
/// The typed-confirm gate runs here, LAST, after the ordinary `y/N` and after every other refusal
/// has had its say: a prompt asking somebody to retype a risk-ceiling key is the wrong thing to
/// print at a session that turns out to have no control connection at all.
fn run_set_setting(
    session: &Session<'_>,
    rl: &mut DefaultEditor,
    file: &str,
    key: &str,
    value: &str,
    reason: Option<&str>,
) {
    // THE TYPED CONFIRM (`WireCommand::SetSetting`'s contract). It is asked for `policy.toml` and
    // nothing else, and it is asked EVEN UNDER `--yes` — see this module's doc.
    let confirm = if is_policy_file(file) {
        match typed_key_confirm(rl, key) {
            Some(typed) => Some(typed),
            None => {
                println!("aborted — nothing was sent");
                return;
            }
        }
    } else {
        None
    };

    let Some((control_key, _)) = session.keys.control() else {
        // Unreachable: `run_write`'s gate proved a live control connection, which needs a key.
        println!("{}", NoControl::NoKey.refusal_line());
        return;
    };
    match vike_tradehub_client::set_setting(
        session.node.as_str(),
        control_key.as_bytes(),
        file,
        key,
        value,
        confirm.as_deref(),
        reason,
    ) {
        Ok(true) => println!(
            "written to {file}: {key} = {value} — the RUNNING node keeps its boot-time value; \
             restart it for this to take effect (every policy.toml key is in this class)"
        ),
        Ok(false) => println!(
            "written to {file}: {key} = {value} — HOT-APPLIED by the node, no restart needed"
        ),
        // The three kinds below are the node saying no BEFORE anything reached its disk: a
        // capability it does not advertise, an auth denial, and its own loader refusing the
        // would-be file. Everything else is transport, and transport is where the honest answer is
        // "unknown" — the request may have been written and the reply lost, exactly the asymmetry
        // the order path's `Disconnected` arm reasons about.
        Err(e) => match e.kind() {
            std::io::ErrorKind::Unsupported
            | std::io::ErrorKind::PermissionDenied
            | std::io::ErrorKind::InvalidData => {
                println!("not written: {e} — nothing landed on the node's disk");
            }
            _ => println!(
                "[error] NOT CONFIRMED: {e} — the write may or may not have landed. Re-read that \
                 key on the node before retrying, so a retry cannot be a second write."
            ),
        },
    }
}

/// Ask the operator to RETYPE `key` and return what they typed, or `None` for "abort".
///
/// `Some` only on an exact match. Two reasons this compares here instead of letting the node do it:
/// a mismatch is then a refusal that costs no round trip and no audit record, and — the part that
/// matters — the value sent is a value the operator TYPED. That is the whole of
/// `WireCommand::SetSetting`'s contract: the client may never pre-fill this field from the `key` it
/// already parsed, because the friction IS the protection. An empty line, a mismatch, Ctrl-C and
/// Ctrl-D are all "abort".
///
/// ⚠ The prompt deliberately does NOT echo the key: it is on the preview line directly above and
/// the operator typed it on the command line, so repeating it here would put the answer next to the
/// question and turn a retype into a copy.
fn typed_key_confirm(rl: &mut DefaultEditor, key: &str) -> Option<String> {
    println!(
        "  this is a POLICY write: the node refuses it unless you retype the key EXACTLY (it holds \
         the risk ceilings, so --yes cannot answer for you)"
    );
    let typed = rl.readline("retype the key to confirm: ").ok()?;
    let typed = typed.trim();
    if typed == key {
        Some(typed.to_string())
    } else {
        if !typed.is_empty() {
            println!("that is not the key");
        }
        None
    }
}

/// Does `file` name the node's `policy.toml`? The bare stem is accepted by the node, so it is
/// accepted here too, and the match is case-insensitive.
///
/// ⚠ It errs SAFE in both directions, which is why an approximate answer is tolerable: a spelling
/// this misses but the node recognises reaches the node with `confirm: None` and is REFUSED there
/// (nothing is written); a spelling this claims but the node does not costs one needless retype.
/// The node is the authority on which file a name resolves to — this classification exists only to
/// decide whether to ask.
fn is_policy_file(file: &str) -> bool {
    let named = file.trim().to_ascii_lowercase();
    named == "policy" || named == "policy.toml"
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
// re-exported above. Only the REPL line GRAMMAR stays here, for BOTH grammars: the order verbs
// ([`parse_line`]) and the node-lifecycle three ([`parse_lifecycle`]), which used to build their
// own wire commands here and no longer do.

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
    let (command, tail) = split_tail(line, REASON_FLAG);
    let reason = tail.map(unquote).map(str::trim).filter(|r| !r.is_empty()).map(str::to_string);
    (command, reason)
}

/// Split `line` at the first whole-token occurrence of `flag`: everything BEFORE it (with trailing
/// whitespace trimmed) and everything AFTER it (trimmed), or `None` when the flag is not present.
///
/// The shared mechanic behind BOTH rest-of-line flags — [`split_reason`]'s `--reason` and
/// [`parse_mount`]'s `--params` — and it exists as one function because they are the same rule:
/// take the remainder verbatim so a multi-word value (a sentence, a JSON object with spaces in it)
/// needs no quoting, and match the flag only as a whole whitespace-delimited token so a coid or a
/// symbol that merely CONTAINS the text is not a split point.
///
/// ⚠ It distinguishes ABSENT (`None`) from PRESENT-BUT-EMPTY (`Some("")`), which its two callers
/// answer differently and must: an empty `--reason` is no rationale (there is nothing to record),
/// while an empty `--params` is a typo — the operator asked for a params object and supplied none,
/// and silently mounting with `{}` would start a strategy on defaults they never chose. PURE.
fn split_tail<'a>(line: &'a str, flag: &str) -> (&'a str, Option<&'a str>) {
    let mut cursor = 0usize;
    let at = loop {
        let Some(hit) = line[cursor..].find(flag) else { return (line, None) };
        let at = cursor + hit;
        let after = &line[at + flag.len()..];
        let is_token = (at == 0 || line[..at].ends_with(char::is_whitespace))
            && (after.is_empty() || after.starts_with(char::is_whitespace));
        if is_token {
            break at;
        }
        cursor = at + flag.len();
    };
    (line[..at].trim_end(), Some(line[at + flag.len()..].trim()))
}

/// Strip ONE layer of matching surrounding `"` or `'` quotes, if present. Byte indexing is safe: both
/// quote characters are single-byte, so slicing just inside them lands on char boundaries.
fn unquote(s: &str) -> &str {
    let b = s.as_bytes();
    let quoted = b.len() >= 2 && b[0] == b[b.len() - 1] && (b[0] == b'"' || b[0] == b'\'');
    if quoted { &s[1..s.len() - 1] } else { s }
}

/// Parse one REPL line, routing on its leading token: the strategy-lifecycle grammar
/// ([`parse_lifecycle`]) if it claims the verb, the order grammar ([`parse_line`]) otherwise. The
/// ONE entry the REPL loop calls, so a line still has exactly one grammar even though the file
/// holds two. PURE.
///
/// ⚠ It used to return a `Parsed` union — `Parsed::Order(Verb)` beside a
/// `Parsed::Lifecycle(WireCommand)` — because the lifecycle three were built here rather than in
/// [`crate::cmd::verbs`]. Both grammars produce a [`Verb`] now, so the union is gone and there is
/// one write path from here down; see this module's doc for what happened to the second builder.
///
/// The line handed here has already had any `--reason` tail split off by [`split_reason`].
pub fn parse_repl_line(line: &str) -> Result<Verb, String> {
    let Some((verb, rest)) = take_token(line) else {
        return Err("empty line".to_string());
    };
    match parse_lifecycle(verb, rest) {
        Some(parsed) => parsed,
        None => parse_line(line),
    }
}

/// The strategy-LIFECYCLE grammar — `mount` / `unmount` / `set-setting` — parsed to the [`Verb`]
/// each names. `None` when `verb` is not one of them, which is the signal [`parse_repl_line`] falls
/// through to the order vocabulary on.
///
/// `rest` is the remainder of the line VERBATIM, not a token slice, because two of these verbs take
/// a rest-of-line value (`mount --params`, `set-setting`'s value) that whitespace tokenization
/// would mangle — a JSON object and a TOML array both contain spaces a person will type. That is
/// the whole of what stays REPL-specific here: the grammar. The [`WireCommand`] each one becomes is
/// built by [`Verb::to_wire_command`], the same site the `mcp` write tools resolve through.
fn parse_lifecycle(verb: &str, rest: &str) -> Option<Result<Verb, String>> {
    match verb.to_ascii_lowercase().as_str() {
        "mount" => Some(parse_mount(rest)),
        "unmount" => Some(parse_unmount(rest)),
        // `set` is sugar the way `panic`/`pos`/`snap` are, and it reads naturally at a prompt. It
        // cannot collide with the `state` verb: they differ in the first three characters.
        "set-setting" | "set" => Some(parse_set_setting(rest)),
        _ => None,
    }
}

/// The `--params` marker on `mount`: like `--reason`, everything after it is taken to end of line.
const PARAMS_FLAG: &str = "--params";

/// `mount <venue> <symbol> <interval> (--name <strategy> | --rhai <path>) [--id <mount-id>]
/// [--params <json to end of line>]`
///
/// Adds a strategy to the RUNNING node's core with no restart ([`Verb::MountStrategy`], which
/// becomes `WireCommand::MountStrategy` at the shared construction site).
///
/// **`--name` XOR `--rhai`.** The wire contract is an exclusive choice — the profile `[strategy]`
/// vocabulary verbatim, a registry name or a Rhai script path — so both-or-neither is refused HERE,
/// naming both spellings, rather than spending a round trip on a refusal the grammar already knew.
/// ⚠ `--rhai` is a path on the **NODE's** filesystem, not this machine's: the node opens it, and a
/// path that exists here proves nothing about there.
///
/// **`--params` runs to end of line** and must be a JSON OBJECT — the `[strategy.params]` table, in
/// the same delegate-don't-mirror form `WireCommand::MountStrategy` carries, so the wire never
/// re-declares a strategy's knobs and neither does this parser. Rest-of-line rather than one token
/// because `{"gamma": 0.0008}` is what a person types and whitespace tokenization would split it
/// into three. It therefore comes LAST among the mount flags (a trailing `--reason` is still fine —
/// that tail is split off before this parser ever runs). Absent ⇒ `{}`, an empty table; PRESENT and
/// empty is a typo and is refused, because mounting on a strategy's defaults nobody chose is not
/// what the operator asked for.
fn parse_mount(rest: &str) -> Result<Verb, String> {
    const USAGE: &str = "usage: mount <venue> <symbol> <interval> (--name <strategy> | --rhai \
                         <path-on-the-node>) [--id <mount-id>] [--params <json to end of line>]";

    let (head, params_text) = split_tail(rest, PARAMS_FLAG);
    let params = match params_text {
        None => json!({}),
        Some("") => {
            return Err(format!(
                "mount: {PARAMS_FLAG} needs a JSON object — drop the flag entirely for an empty \
                 params table\n{USAGE}"
            ));
        }
        Some(text) => {
            let value: Value = serde_json::from_str(text)
                .map_err(|e| format!("mount: {PARAMS_FLAG} is not JSON ({e}): {text}\n{USAGE}"))?;
            if !value.is_object() {
                return Err(format!(
                    "mount: {PARAMS_FLAG} must be a JSON OBJECT (the `[strategy.params]` table), \
                     got {value}\n{USAGE}"
                ));
            }
            value
        }
    };

    let (mut name, mut rhai, mut id): (Option<String>, Option<String>, Option<String>) =
        (None, None, None);
    let mut positional: Vec<&str> = Vec::new();
    let tokens: Vec<&str> = head.split_whitespace().collect();
    let mut i = 0usize;
    while i < tokens.len() {
        let tok = tokens[i];
        // Both spellings, the same pair `submit --coid` accepts: `--flag value` and `--flag=value`.
        let (flag, inline) = match tok.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v)),
            _ => (tok, None),
        };
        match flag {
            "--name" | "--rhai" | "--id" => {
                let value = match inline {
                    Some(v) => {
                        i += 1;
                        v
                    }
                    None => {
                        let v = *tokens
                            .get(i + 1)
                            .ok_or_else(|| format!("mount: {flag} needs a value\n{USAGE}"))?;
                        i += 2;
                        v
                    }
                };
                // A value that is itself a flag means the operator's value went missing and the
                // NEXT flag was eaten as one — the failure `submit --coid has space` taught.
                if value.is_empty() || value.starts_with("--") {
                    return Err(format!("mount: {flag} needs a value, got {value:?}\n{USAGE}"));
                }
                let slot = match flag {
                    "--name" => &mut name,
                    "--rhai" => &mut rhai,
                    _ => &mut id,
                };
                if slot.is_some() {
                    return Err(format!("mount: {flag} given twice\n{USAGE}"));
                }
                *slot = Some(value.to_string());
            }
            other if other.starts_with("--") => {
                return Err(format!("mount: unexpected flag {other:?}\n{USAGE}"));
            }
            _ => {
                positional.push(tok);
                i += 1;
            }
        }
    }

    if positional.len() != 3 {
        return Err(USAGE.to_string());
    }
    match (&name, &rhai) {
        (Some(_), Some(_)) => {
            return Err(format!(
                "mount: --name and --rhai are EXCLUSIVE — a mount's strategy source is either a \
                 registry name or a Rhai script path, never both\n{USAGE}"
            ));
        }
        (None, None) => {
            return Err(format!(
                "mount: a strategy source is required — pass --name <strategy> (a registry name) \
                 or --rhai <path> (a script on the NODE's filesystem)\n{USAGE}"
            ));
        }
        _ => {}
    }

    Ok(Verb::MountStrategy {
        venue: positional[0].to_string(),
        symbol: positional[1].to_string(),
        interval: positional[2].to_string(),
        // `None` lets the node derive `{venue}__{symbol}__{interval}`. Deliberately NOT derived
        // here: the derivation is the node's rule, and a client copy of it would be a second one to
        // keep in step for no gain — the node accepts the absence and answers with its own answer.
        controller_id: id,
        name,
        rhai,
        params,
    })
}

/// `unmount <mount-id>` — remove one mount from the running core ([`Verb::UnmountStrategy`]).
///
/// One token, and a second one is an error rather than being ignored: a mount id carries no spaces,
/// so extra words mean the operator meant something this verb does not do — and the thing they most
/// plausibly meant (naming the mount by `<venue> <symbol> <interval>`) would need this client to
/// re-implement the node's own id derivation. `help` states where the id comes from instead.
fn parse_unmount(rest: &str) -> Result<Verb, String> {
    const USAGE: &str = "usage: unmount <mount-id>";
    let mut tokens = rest.split_whitespace();
    let id = tokens.next().ok_or(USAGE)?;
    if tokens.next().is_some() {
        return Err(format!(
            "unmount: a mount id is ONE token (the explicit --id given at mount time, or the \
             node-derived {{venue}}__{{symbol}}__{{interval}})\n{USAGE}"
        ));
    }
    Ok(Verb::UnmountStrategy { controller_id: id.to_string() })
}

/// `set-setting <file> <full.dotted.key> <value to end of line>` — write ONE key into ONE of the
/// node's four settings files ([`Verb::SetSetting`]).
///
/// The value runs to END OF LINE and is taken VERBATIM. Both halves of that are deliberate: a TOML
/// value can contain spaces (`["a", "b"]`, a prose string), and unlike `--reason` it is NOT
/// unquoted, because quotes are meaningful to the node's TOML parse — stripping them would turn the
/// string `"250"` into the integer `250`, silently changing the type of the key being set.
///
/// ⚠ **`confirm` is `None` here and can never be anything else.** That is this surface's half of
/// `WireCommand::SetSetting`'s typed-confirm contract expressed structurally rather than as a rule
/// somebody must remember: the parser has the `key` in hand and could trivially fill the field, and
/// a policy write would then be one keystroke — exactly what the contract exists to prevent. The
/// only thing that fills it is [`typed_key_confirm`], from what the operator retypes at the prompt.
///
/// ⚠ It is a STRONGER claim than the `mcp` server's twin, and deliberately so: that parser CAN
/// produce a `confirm` (it copies the `policy_confirm` argument an operator handed the agent), and
/// a policy write with none is REFUSED there rather than prompted for. Only the construction is
/// shared — see this module's lifecycle section, and `crate::cmd::verbs`' module doc.
///
/// Nothing here validates the FILE name or the key's first segment against it. The node holds the
/// loader and refuses with its own message; a second copy of that rule on this side would be one
/// more thing to keep in step, and it would still not be the one that decides.
fn parse_set_setting(rest: &str) -> Result<Verb, String> {
    const USAGE: &str = "usage: set-setting <policy|config|preferences|flags>[.toml] \
                         <full.dotted.key> <value to end of line>";
    let (file, after) = take_token(rest).ok_or(USAGE)?;
    let (key, value) = take_token(after).ok_or(USAGE)?;
    // Trailing whitespace is stripped and surrounding quotes are NOT (see the doc above) — the two
    // are different acts: one removes what the terminal added, the other would change the type.
    let value = value.trim_end();
    if value.is_empty() {
        return Err(format!(
            "set-setting: no value — a key set to nothing is not a write anybody meant\n{USAGE}"
        ));
    }
    Ok(Verb::SetSetting {
        file: file.to_string(),
        key: key.to_string(),
        value: value.to_string(),
        confirm: None,
    })
}

/// Split off the first whitespace-delimited token and return it with the (left-trimmed) remainder,
/// or `None` when there is no token at all. The half-tokenized read the two rest-of-line verbs need:
/// their leading fields are tokens, their last field is not.
fn take_token(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    if s.is_empty() {
        return None;
    }
    match s.find(char::is_whitespace) {
        Some(at) => Some((&s[..at], s[at..].trim_start())),
        None => Some((s, "")),
    }
}

/// Parse one REPL line into a [`Verb`] — the ORDER/state vocabulary. Terse, whitespace-tokenized.
/// Malformed input is a clean `Err(String)`. PURE — no network, no I/O — so the whole grammar is
/// unit-testable.
///
/// Reached through [`parse_repl_line`], which routes the strategy-lifecycle verbs elsewhere first.
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
            ));
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
        // The B5 mount verbs — spelled by [`parse_mount`] / [`parse_unmount`] since the lifecycle
        // grammar landed. `params` renders as the JSON object that goes on the wire, not a summary
        // of it: this line is the last thing between the operator and a strategy trading their
        // account, and a preview that elides the knobs is a preview of a different mount.
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
        // The REQ-7 settings write — spelled by [`parse_set_setting`] since the lifecycle grammar
        // landed. Show the assignment and WHETHER a typed confirm rides along, never that confirm's
        // own text. ⚠ At preview time it is always absent: the typed confirm is collected AFTER the
        // `y/N` gate ([`run_set_setting`]), so `[confirmed]` renders for a command another caller
        // minted, not for one this REPL is about to send.
        WireCommand::SetSetting { file, key, value, confirm } => {
            format!(
                "SET-SETTING {file} {key} = {value}{}",
                if confirm.is_some() { " [confirmed]" } else { "" }
            )
        }
    }
}

fn side_word(side: i32) -> &'static str {
    if side >= 0 { "buy" } else { "sell" }
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
    println!("  STRATEGY LIFECYCLE (preview + confirm; the node must speak these verbs):");
    println!(
        "    mount <venue> <symbol> <interval> (--name <strategy> | --rhai <path-on-the-node>) \
         [--id <mount-id>] [--params <json>]"
    );
    println!(
        "      (--name XOR --rhai; --params is a JSON object and runs to end of line, so put it \
         last; --rhai is a path on the NODE)"
    );
    println!(
        "    unmount <mount-id>           (the --id given at mount time, else the node-derived \
         {{venue}}__{{symbol}}__{{interval}})"
    );
    // SAY WHAT UNMOUNT DOES NOT DO. "unmount" reads to a person under pressure as "get me out of
    // this", and it is not: the node cancels that mount's live ORDERS and saves its state, and
    // leaves every POSITION open and now unattended. An operator who learns that from the position
    // table afterwards learned it too late.
    println!(
        "      (the node cancels that mount's live orders and saves its state; POSITIONS ARE NOT \
         FLATTENED — use `flatten`/`market-exit` for that)"
    );
    println!(
        "    set-setting <policy|config|preferences|flags>[.toml] <full.dotted.key> <value to end \
         of line>"
    );
    // The typed confirm is the one place `--yes` stops working, so the help says so rather than
    // letting a scripted session discover it at a prompt that never gets answered.
    println!(
        "      (validated by the node's own loader before anything lands; a policy.toml write also \
         makes you RETYPE the key, which --yes cannot answer)"
    );
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
        assert!(line.contains("node-key store"), "it must name BOTH places we looked: {line}");
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

    // ---- the strategy-lifecycle grammar (mount / unmount / set-setting) ----------------------

    /// The one entry the REPL loop calls, resolved through the SHARED construction site to the
    /// wire command it produces — which is what the assertions below are about, and what
    /// [`run_write`] is handed.
    fn lifecycle(line: &str) -> WireCommand {
        let verb = parse_repl_line(line)
            .unwrap_or_else(|e| panic!("expected a lifecycle verb for {line:?}, got error: {e}"));
        assert!(verb.is_write(), "{line:?} must parse to a WRITE verb, got {verb:?}");
        verb.to_wire_command()
            .unwrap_or_else(|| panic!("{line:?}: a write verb must build a wire command"))
    }

    /// The router keeps ONE grammar per line: the lifecycle verbs are claimed by
    /// [`parse_lifecycle`] and land on their own [`Verb`] variants, everything else still falls
    /// through to the order vocabulary untouched, and an unknown verb is still the ORDER grammar's
    /// own error (the lifecycle half must not swallow it).
    ///
    /// ⚠ This used to assert on a `Parsed::Lifecycle` / `Parsed::Order` union. With both grammars
    /// producing one [`Verb`], the routing claim is made on the VARIANT instead — the same
    /// property, asserted one level down, and still the thing that would break if the lifecycle
    /// half started claiming (or stopped claiming) a token.
    #[test]
    fn parse_repl_line_routes_each_verb_to_exactly_one_grammar() {
        for line in [
            "mount sim BTCUSDT 1m --name spread_maker",
            "unmount grid-a",
            "set-setting flags.toml flags.reconcile_off true",
            "set flags.toml flags.reconcile_off true",
        ] {
            assert!(
                matches!(
                    parse_repl_line(line),
                    Ok(Verb::MountStrategy { .. }
                        | Verb::UnmountStrategy { .. }
                        | Verb::SetSetting { .. })
                ),
                "{line:?} must be claimed by the lifecycle grammar"
            );
        }
        for line in ["submit sim BTCUSDT buy 1", "orders", "state halted", "quit"] {
            assert!(
                !matches!(
                    parse_repl_line(line),
                    Ok(Verb::MountStrategy { .. }
                        | Verb::UnmountStrategy { .. }
                        | Verb::SetSetting { .. })
                ),
                "{line:?} must fall through to the order vocabulary"
            );
            // …and PARSE there. Without this the negative above would also be satisfied by an
            // error, which is the one way "fell through" could be true and useless.
            assert!(parse_repl_line(line).is_ok(), "{line:?} must still parse");
        }
        // The order grammar still owns the unknown-verb error, and `quit` still round-trips as the
        // value the REPL loop breaks on.
        assert!(parse_repl_line("frobnicate").is_err());
        assert!(parse_repl_line("   ").is_err());
        assert_eq!(parse_repl_line("quit").unwrap(), Verb::Quit);
    }

    /// The minimal registry-name mount: no explicit id (the node derives one), no params (an empty
    /// table).
    #[test]
    fn mount_by_registry_name_defaults_the_id_and_the_params() {
        assert_eq!(
            lifecycle("mount binance BTCUSDT 1m --name spread_maker"),
            WireCommand::MountStrategy {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                interval: "1m".into(),
                controller_id: None,
                name: Some("spread_maker".into()),
                rhai: None,
                params: json!({}),
            }
        );
    }

    /// The Rhai form, with every optional field supplied — and `--params` carrying the SPACES a
    /// person actually types, which is the whole reason it runs to end of line rather than being
    /// one whitespace-delimited token.
    #[test]
    fn mount_by_rhai_path_takes_an_id_and_a_spacey_params_object() {
        assert_eq!(
            lifecycle(
                "mount sim ETHUSDT 5m --rhai /srv/vike-<unit>/strategies/grid.rhai --id grid-a \
                 --params {\"gamma\": 0.0008, \"levels\": 4}"
            ),
            WireCommand::MountStrategy {
                venue: "sim".into(),
                symbol: "ETHUSDT".into(),
                interval: "5m".into(),
                controller_id: Some("grid-a".into()),
                name: None,
                rhai: Some("/srv/vike-<unit>/strategies/grid.rhai".into()),
                params: json!({"gamma": 0.0008, "levels": 4}),
            }
        );
        // The `--flag=value` spelling is the same pair `submit --coid` accepts.
        assert_eq!(
            lifecycle("mount sim ETHUSDT 5m --name=spread_maker --id=grid-b"),
            WireCommand::MountStrategy {
                venue: "sim".into(),
                symbol: "ETHUSDT".into(),
                interval: "5m".into(),
                controller_id: Some("grid-b".into()),
                name: Some("spread_maker".into()),
                rhai: None,
                params: json!({}),
            }
        );
    }

    /// **The XOR, refused at PARSE time and naming BOTH spellings.** `WireCommand::MountStrategy`'s
    /// contract is an exclusive choice; a client that let the node discover the violation spends a
    /// round trip to say what the grammar already knew, and answers with the node's words rather
    /// than with the two flags the operator has to choose between.
    #[test]
    fn mount_refuses_both_or_neither_strategy_source_and_names_the_two_spellings() {
        for line in
            ["mount sim BTCUSDT 1m --name spread_maker --rhai /tmp/s.rhai", "mount sim BTCUSDT 1m"]
        {
            let err = parse_repl_line(line).expect_err(line);
            assert!(err.contains("--name"), "{line:?} must name --name: {err}");
            assert!(err.contains("--rhai"), "{line:?} must name --rhai: {err}");
        }
    }

    /// `--params` is the `[strategy.params]` TABLE, so a non-object (or unparseable) payload is a
    /// clean refusal rather than something the node has to decode and reject. An EMPTY `--params`
    /// is refused too and is not the same as omitting the flag: mounting a strategy on defaults
    /// nobody chose is not what "I typed --params" meant.
    #[test]
    fn mount_params_must_be_a_json_object_and_an_empty_flag_is_not_an_empty_table() {
        let base = "mount sim BTCUSDT 1m --name spread_maker";
        assert!(parse_repl_line(&format!("{base} --params [1,2]")).is_err(), "an array");
        assert!(parse_repl_line(&format!("{base} --params 7")).is_err(), "a scalar");
        assert!(parse_repl_line(&format!("{base} --params {{oops")).is_err(), "not JSON");
        let err = parse_repl_line(&format!("{base} --params")).expect_err("empty --params");
        assert!(err.contains("--params"), "{err}");
        // …while OMITTING it is the empty table, and always was.
        let WireCommand::MountStrategy { params, .. } = lifecycle(base) else { panic!("a mount") };
        assert_eq!(params, json!({}));
    }

    #[test]
    fn mount_rejects_malformed_argument_shapes() {
        assert!(parse_repl_line("mount sim BTCUSDT --name spread_maker").is_err(), "no interval");
        assert!(parse_repl_line("mount a b c d --name s").is_err(), "a fourth positional");
        assert!(parse_repl_line("mount a b c --name").is_err(), "dangling value");
        // A flag eaten as another flag's value is the `submit --coid has space` failure again.
        assert!(parse_repl_line("mount a b c --id --name s").is_err(), "a flag as a value");
        assert!(parse_repl_line("mount a b c --name s --name t").is_err(), "twice");
        assert!(parse_repl_line("mount a b c --name s --bogus 1").is_err(), "unknown flag");
    }

    /// `unmount` takes ONE token. A second is refused rather than ignored, and the message points
    /// at where a mount id comes from — this client deliberately does not re-derive the node's
    /// `{venue}__{symbol}__{interval}` rule.
    #[test]
    fn unmount_takes_one_mount_id() {
        assert_eq!(
            lifecycle("unmount grid-a"),
            WireCommand::UnmountStrategy { controller_id: "grid-a".into() }
        );
        assert_eq!(
            lifecycle("unmount binance__BTCUSDT__1m"),
            WireCommand::UnmountStrategy { controller_id: "binance__BTCUSDT__1m".into() }
        );
        assert!(parse_repl_line("unmount").is_err());
        assert!(parse_repl_line("unmount binance BTCUSDT 1m").is_err());
    }

    /// The value runs to END OF LINE and is taken VERBATIM — quotes included, because they are what
    /// tells the node's TOML parse that `"250"` is a string and not the integer `250`.
    #[test]
    fn set_setting_takes_the_value_to_end_of_line_without_unquoting_it() {
        assert_eq!(
            lifecycle("set-setting policy.toml policy.max_notional_per_order 250"),
            WireCommand::SetSetting {
                file: "policy.toml".into(),
                key: "policy.max_notional_per_order".into(),
                value: "250".into(),
                confirm: None,
            }
        );
        // Spaces survive: a TOML array is a value a person types with spaces in it.
        let WireCommand::SetSetting { value, .. } =
            lifecycle("set config.toml config.venues [\"binance\", \"okx\"]")
        else {
            panic!("a settings write")
        };
        assert_eq!(value, "[\"binance\", \"okx\"]");
        // …and the quotes are NOT stripped, unlike `--reason`'s one forgiving layer.
        let WireCommand::SetSetting { value, .. } =
            lifecycle("set-setting config.toml config.tradehub_addr \"127.0.0.1:7979\"")
        else {
            panic!("a settings write")
        };
        assert_eq!(value, "\"127.0.0.1:7979\"");

        assert!(parse_repl_line("set-setting policy.toml").is_err(), "no key, no value");
        assert!(parse_repl_line("set-setting policy.toml policy.x").is_err(), "no value");
        assert!(parse_repl_line("set-setting").is_err());
    }

    /// **The typed-confirm contract, held STRUCTURALLY.** The parser has the key in hand and could
    /// fill `confirm` in one line — which would make a `policy.toml` write a single keystroke and
    /// defeat the whole point of the field. It never does, for any file; the only thing that fills
    /// it is [`typed_key_confirm`], from what the operator retypes.
    ///
    /// ⚠ Asserted on the PARSER's own [`Verb`], not on the wire command it becomes: the claim is
    /// about what this grammar can produce, and the shared construction site downstream now carries
    /// a `confirm` for the `mcp` surface (which CAN supply one). `crate::cmd::verbs`' own
    /// `the_typed_confirm_is_never_derived_from_the_key` is that surface's weaker twin.
    #[test]
    fn the_parser_can_never_pre_fill_the_typed_confirm() {
        for line in [
            "set-setting policy.toml policy.max_notional_per_order 250",
            "set-setting flags.toml flags.reconcile_off true",
            "set preferences preferences.theme dark",
        ] {
            let Ok(Verb::SetSetting { confirm, .. }) = parse_repl_line(line) else {
                panic!("a settings write: {line}")
            };
            assert_eq!(confirm, None, "the parser must never mint a confirm: {line}");
        }
    }

    /// Which file gets the retype prompt. It errs SAFE in both directions by design — see
    /// [`is_policy_file`] — so what is pinned here is that every spelling the node accepts for the
    /// POLICY file is recognised, and that no other settings file is.
    #[test]
    fn only_the_policy_file_asks_for_a_typed_confirm() {
        for named in ["policy.toml", "policy", "POLICY.TOML", "Policy"] {
            assert!(is_policy_file(named), "{named} is the policy file");
        }
        for named in ["config.toml", "flags.toml", "preferences.toml", "config", "policies.toml"] {
            assert!(!is_policy_file(named), "{named} is not the policy file");
        }
    }

    /// `--reason` still runs to end of line, and it composes with `--params`, which does too: the
    /// rationale is split off FIRST, so the mount parser never sees it and the JSON keeps its
    /// spaces.
    #[test]
    fn a_reason_tail_composes_with_the_rest_of_line_params() {
        let (line, why) = split_reason(
            "mount sim BTCUSDT 1m --name spread_maker --params {\"gamma\": 0.0008} --reason \
             trialling a tighter quote",
        );
        assert_eq!(why.as_deref(), Some("trialling a tighter quote"));
        let WireCommand::MountStrategy { params, name, .. } = lifecycle(line) else {
            panic!("a mount")
        };
        assert_eq!(params, json!({"gamma": 0.0008}));
        assert_eq!(name.as_deref(), Some("spread_maker"));
    }

    /// The shared rest-of-line splitter distinguishes ABSENT from PRESENT-AND-EMPTY, which is the
    /// one property its two callers answer differently: an empty `--reason` is no rationale, an
    /// empty `--params` is a typo.
    #[test]
    fn split_tail_separates_an_absent_flag_from_an_empty_one() {
        assert_eq!(split_tail("mount a b c", PARAMS_FLAG), ("mount a b c", None));
        assert_eq!(split_tail("mount a b c --params", PARAMS_FLAG), ("mount a b c", Some("")));
        assert_eq!(
            split_tail("mount a b c --params   {\"x\": 1}", PARAMS_FLAG),
            ("mount a b c", Some("{\"x\": 1}"))
        );
        // Whole-token only — a symbol that merely CONTAINS the text is not a split point.
        assert_eq!(split_tail("mount a --paramsy c", PARAMS_FLAG), ("mount a --paramsy c", None));
    }

    /// The preview is the last thing between an operator and a running strategy, so it must show
    /// the mount's IDENTITY, its SOURCE and its knobs — and, for a settings write, the assignment
    /// without the typed confirm's own text.
    #[test]
    fn the_lifecycle_previews_carry_what_the_operator_has_to_check() {
        let preview = describe_command(&lifecycle(
            "mount binance BTCUSDT 1m --name spread_maker --id grid-a --params {\"gamma\": 0.5}",
        ));
        for needle in ["binance", "BTCUSDT", "1m", "grid-a", "spread_maker", "gamma"] {
            assert!(preview.contains(needle), "the preview must carry {needle}: {preview}");
        }
        // A mount with no explicit id shows the ABSENCE rather than inventing the node's derived
        // one, and a Rhai mount shows the script path as its source.
        let preview = describe_command(&lifecycle("mount sim ETHUSDT 5m --rhai /srv/s.rhai"));
        assert!(preview.contains("id=-"), "no id was given: {preview}");
        assert!(preview.contains("/srv/s.rhai"), "the source is the script path: {preview}");

        let preview = describe_command(&lifecycle("unmount grid-a"));
        assert!(preview.contains("UNMOUNT-STRATEGY") && preview.contains("grid-a"), "{preview}");

        // At preview time the typed confirm has not been collected yet — it is asked for AFTER the
        // y/N gate — so the marker is absent, and the assignment is fully visible.
        let preview = describe_command(&lifecycle(
            "set-setting policy.toml policy.max_notional_per_order 250",
        ));
        assert!(
            preview.contains("policy.max_notional_per_order") && preview.contains("250"),
            "{preview}"
        );
        assert!(!preview.contains("[confirmed]"), "nothing is confirmed yet: {preview}");
    }
}
