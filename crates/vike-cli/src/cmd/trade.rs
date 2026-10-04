//! `vike-cli trade` — the trade PLANE's entry point, over a running vike-tradehub node (headless
//! two-layer plan, Layer 1): one-shot verbs live under a REQUIRED group ([`plane::GROUPS`] —
//! `order` | `position` | `strategy` built; `account` | `watch` named in the roster and refused as
//! designed-not-built), `status` / `halt` / `resume` are three NODE-WIDE, risk-REDUCING words that
//! take no book and so take no group, and — with no leading verb at all — this opens a HUMAN
//! interactive REPL, the terse, terminal-driven sibling of the `mcp` trade tools: inside it a person
//! types short verbs (`submit`, `cancel`, `flatten`, `orders`, `positions`, `status`, …) and this
//! dispatches them to the node's authenticated OBSERVE (read) and CONTROL (write) servers via the
//! LIGHT wire crate (`vike_tradehub_client::{RemoteCoreHandle, RemoteControlHandle}`). Part of the
//! "vike-cli as the one agent surface" program
//! (`docs/superpowers/specs/2026-07-26-vike-cli-agent-surface-program.md`).
//!
//! ⚠ **This module doc opened by calling the whole surface "a HUMAN interactive REPL" until the
//! 2026-09-21 trade-CLI-plane design landed**
//! (`docs/superpowers/specs/2026-09-21-trade-cli-surface-design.md`). That was complete the day it
//! was written; the one-shot GROUP verbs are the newer half, and the REPL is now what a BARE
//! invocation opens rather than the whole of what this file is.
//!
//! # `status` / `halt` / `resume` — the same three words on both sides of the prompt
//!
//! `vike-cli trade` with a leading VERB runs that one operation and exits; with none, it opens the
//! REPL. `status` is the READ ([`crate::cmd::trade::status`] owns it, and its module doc carries the
//! two wire reads it merges); `halt` and `resume` are the two WRITES, each named for what it does.
//! Ruling 16 of `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` is why they
//! are the SAME words in both places: one word for one operation, so nothing has to be re-learned
//! at the prompt.
//!
//! ⚠ **This replaces `state`, and the replacement is a safety change rather than a rename.**
//! `parse_line` used to hold `"state" => match rest.first() { None => ShowState, Some(s) =>
//! SetState(parse_state(s)?) }`: `state` PRINTED the mode and `state halted` HALTED A LIVE DAEMON,
//! with one token between them. That is a destructive action wearing a read verb's argument — the
//! shape a tab-completion, a pasted line, or an operator typing `state` and then a word to "filter"
//! turns into a stopped node. Ruling 17 removes `state` entirely (no alias, no deprecation shim —
//! this workspace's no-shims rule) and gives each write its own word. The exposure was never on the
//! wire (`Scope::Read` cannot send `SetState`); it was at the CONTROL prompt, where the operator
//! holds the key that CAN halt.
//!
//! ⚠ **`Reducing` has no REPL verb, deliberately, and the ruling's own test for that is met.** A
//! halt does not trap you: `crates/vike-exec/src/risk.rs`'s `check_inner` admits a POSITION-COVERED
//! reduce under `Halted`, so `market-exit`/`flatten` and the re-price of a resting exit all still
//! work — which is the "`Reducing` is reached through `halt`'s behaviour on open positions" the
//! ruling names. `Reducing` itself stays reachable where an automation needs it: the `mcp` surface's
//! `set_trading_state` tool and the node's own Telegram `/state reducing`.
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
//! A node REFUSES a remote `Submit` with an empty `client_order_id` (`vike_tradehub::server::control`'s
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
//! guardrail check over this machine's `policy` notional ceiling (`max_notional_per_order` —
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
//! - `set-setting <full.dotted.key> <value>` writes ONE row of the node's settings database, named
//!   by its key (`docs/decisions/0086`) and validated by the node's own loader before it commits.
//!   It takes the same two positionals `vike-cli config set` takes; the `<file>` token it used to
//!   take first is retired and refused by name ([`parse_set_setting`]).
//!
//! ⚠ **`set-setting` asks ONE question — the `confirm? [y/N]` every write asks, which `--yes`
//! skips.** It used to ask a SECOND: for a policy write, retype the key, even under `--yes`, so a
//! policy write was the one REPL line a script could not complete. `docs/decisions/0086` point 7
//! deleted that ceremony for every key, on every surface (*"confirmation over confirmation … a
//! nightmare"*). What guards a live limit instead is what catches a mistake: the node's loader
//! bounds the value, the preview prints `old → new` read off the node BEFORE the y/N
//! ([`verbs::SettingChange`], shared with the `mcp` server's preview), and the node's journal
//! records who changed it. The wire's `confirm` field stays for an older client, and the shared
//! construction site ([`Verb::to_wire_command`]) always sends `None`.
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
//! The REPL loop, `rustyline`, and the live connection are thin glue over those pure parts, so they
//! are not unit-tested here — the SHIPPED binary is driven over a pipe against a real node instead,
//! by `crates/vike-cli/tests/trade_node_e2e.rs`, which is where the one-question settings write is
//! pinned (`a_policy_set_setting_goes_out_after_one_confirm`).

use std::process::ExitCode;
use std::time::{Duration, Instant};

use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;
use serde_json::{Value, json};
use vike_model::orders::client_order_id::ClientOrderIdGenerator;
use vike_tradehub_client::wire::{WireCommand, WireOrderRequest, WireSnapshot, WireTradingState};
use vike_tradehub_client::{
    CommandOutcome, ControlRejected, RemoteControlHandle, RemoteCoreHandle,
};

use crate::cmd::args::{self, Flags};
use crate::cmd::nodekeys::{self, NodeKeyring};
use crate::cmd::verbs;
use crate::cmd::verbs::Verb;
use crate::exit::Exit;

mod oneshot;
// The read-only node question `vike-cli trade status` answers.
mod order;
mod plane;
mod position;
mod render;
mod selector;
pub mod status;
mod strategy;

/// The plane's own usage roster, in the shape `crate::cmd::data`'s `USAGE` uses: the GROUPS block
/// is DERIVED from [`plane::GROUPS`] (read it there before re-typing this one — the hand-written
/// roster `crate::cmd::data`'s own predecessor used had already omitted a subcommand that shipped
/// months earlier) rather than a second hand-written copy of the same roster.
///
/// ⚠ The trailing note is not decoration. `--json` reached every other non-interactive surface, and
/// a gap with no explanation reads as an oversight somebody will "fix": a machine-readable REPL is
/// what `vike-cli mcp` already is — a whole stdio protocol with a tool schema, session state and a
/// mandatory preview — and a `--json` here would be a second, weaker one to keep in step with it.
/// The line exists so that the absence is a decision a reader can see rather than infer. ⚠ It is
/// KEPT VERBATIM from before this file grew a group layer: the REPL still has no `--json` and
/// `vike-cli mcp` is still the machine surface, and re-wording it here would be the same claim
/// stated twice with two chances to drift.
const USAGE: &str = "\
usage: vike-cli trade <group> <verb> [options]              (one-shot)
       vike-cli trade status --node <host:port> [--json]    (read the mode + what is mounted)
       vike-cli trade halt   --node <host:port> [--yes]     (mode -> Halted)
       vike-cli trade resume --node <host:port> [--yes]     (mode -> Active)
       vike-cli trade --node <host:port> [--yes]            (no group, no verb: opens the REPL)

`status`, `halt` and `resume` take NO group, and that is not a hole in the roster below: a verb
that takes no BOOK takes no group, and all three are node-wide and risk-REDUCING — `halt` under
pressure is one word, not a group plus a word.

GROUPS — every other verb lives in one, and the group is REQUIRED: there is no bare
`vike-cli trade <verb>`, and each pre-group REPL spelling is refused by name with its one-shot
replacement rather than silently accepted:
  order        the working set, and the intent to change it — `trade order --help`
  position     what is held — `trade position --help`
  strategy     what trades by itself — `trade strategy --help`
  account      WHO - which books exist, what is in them, which are LIVE (designed, not built)
  watch        the live stream (designed, not built)

no --json on the REPL: it is interactive, for a human. The MACHINE surface over the same node
is `vike-cli mcp`, which speaks stdio JSON-RPC and exposes the same verbs as tools.";

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
    // ONE-SHOT FIRST. A leading non-flag token is a VERB (`status` / `halt` / `resume`) — ruling 17,
    // and the reason the check is here rather than inside `parse_config` is that the REPL's own
    // grammar takes flags only, so a positional reaching that parser is an "unknown argument" by
    // construction and could never have become a subcommand there. An unrecognised leading word
    // used to be handed BACK to the REPL parser for that "unknown argument" message; it now reaches
    // the GROUP LAYER below instead — see the arm after this match.
    let mut rest = args;
    let first = rest.next();
    match first.as_deref() {
        Some("status") => return crate::cmd::trade::status::run(rest, keys),
        Some("halt") => return run_set_mode_once(rest, keys, WireTradingState::Halted),
        Some("resume") => return run_set_mode_once(rest, keys, WireTradingState::Active),
        // The one-shot twin of [`parse_line`]'s `state` arm: `vike-cli trade state halted` is a
        // line that sat in runbooks and unit files, so the refusal names the words that replaced it
        // rather than reporting `state` as an unknown ARGUMENT — which is what the REPL parser
        // below would otherwise do, correctly and unhelpfully.
        Some("state") => {
            eprintln!("vike-cli trade: {STATE_MIGRATION_HINT}\n{USAGE}");
            return Exit::Usage.into();
        }
        _ => {}
    }
    // THE GROUP LAYER (`plane::claim_group`). `status`/`halt`/`resume`/`state` are the one exemption
    // — node-wide, risk-REDUCING words that take no book and so take no group (`plane`'s own module
    // doc argues why) — everything else the REPL used to accept as a bare positional now lives under
    // a group, and the group is REQUIRED. So a leading token that survives the match above and is
    // NOT a flag is a group claim, never a REPL positional: `parse_config` below takes flags only,
    // and a group name reaching it would misreport as "unknown argument" rather than naming the
    // roster, the retired spelling, or the unbuilt-group notice `claim_group` actually has.
    //
    // ⚠ `first` being `None` (bare `vike-cli trade`) or a FLAG (`--node ...`, `-h`, `-y`) both skip
    // this arm on purpose — a bare invocation and a flags-only one must still reach the REPL grammar
    // below unchanged, exactly as they did before this layer existed.
    if let Some(word) = first.as_deref()
        && !word.starts_with('-')
    {
        return match plane::claim_group(word) {
            // The ORDER group's own task: real verbs now, routed to its own parser/router rather
            // than falling into the REPL grammar below (which takes flags only and would misreport
            // a group word as an unknown argument). `policy_max_notional` is threaded through for
            // the group's WRITE verbs (task 7) — the same ceiling `Session.caps` below resolves for
            // the REPL's own preview guardrail.
            Ok(plane::Group::Order) => order::run(rest, keys, policy_max_notional),
            // Task 6 wired the POSITION and STRATEGY groups' own `ls` verbs, routed the same way;
            // each has since grown WRITE verbs of its own (task 7 for position, task 8 for
            // strategy). Every `plane::Group` variant is matched explicitly now — the placeholder
            // arm this replaced ("a real group, but this build wires no verbs under it yet") has
            // nothing left to cover.
            Ok(plane::Group::Position) => position::run(rest, keys, policy_max_notional),
            // Task 8: `mount`/`unmount` join the strategy group's own `ls` as one-shot writes,
            // needing the same advisory guardrail ceiling `order`/`position`'s write verbs take.
            Ok(plane::Group::Strategy) => strategy::run(rest, keys, policy_max_notional),
            Err(e) => {
                eprintln!("vike-cli trade: {}\n{USAGE}", e.msg);
                e.exit.into()
            }
        };
    }
    let args = first.into_iter().chain(rest);
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
/// authenticated as a credential, and was refused for its SCOPE (`server/handshake.rs`'s `run_handshake`:
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
    /// A key resolved and the node REFUSED the control scope — `flags.tradehub_control`
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
                 all. Turn `flags.tradehub_control` on in ITS settings (`vike-cli config set \
                 flags.tradehub_control true` there, or VIKE_TRADEHUB_CONTROL=1) and restart it \
                 — nothing was sent",
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
/// It borrows the [`NodeKeyring`] rather than copying a key out of it: a settings write needs both
/// keys again — the observe key for the `old → new` read ([`read_setting_change`]) and the control
/// key for [`run_set_setting`], each over a short-lived connection of its own — and a
/// session-lifetime `String` copy of a live node credential is a second place for one to exist for
/// no gain: the keyring is already alive for the whole call and already redacts itself in `Debug`.
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
    // Reads (and `help`).
    match verb {
        Verb::Help => print_verb_help(),
        Verb::Orders(symbol) => with_snapshot(session, |s| print_orders(s, symbol.as_deref())),
        Verb::Positions(venue) => with_snapshot(session, |s| print_positions(s, venue.as_deref())),
        Verb::Equity => with_snapshot(session, print_equity),
        Verb::Snapshot => with_snapshot(session, print_snapshot),
        Verb::Recent(n) => with_snapshot(session, |s| print_recent(s, n)),
        Verb::Status => run_status(session),
        // Writes are handled above; Quit is handled in the loop.
        _ => {}
    }
}

/// `status` at the REPL: the trading MODE from the snapshot this session is already subscribed to,
/// then the mounted-strategy REGISTRY from one short-lived Observe request
/// ([`vike_tradehub_client::strategy_status`]).
///
/// ⚠ **The two halves fail independently and the mode is shown even when the registry read fails.**
/// The one-shot `vike-cli trade status` now behaves the same way — it re-asks for the mode after a
/// registry failure the node could still answer ([`crate::cmd::trade::status`]'s
/// `mode_read_worth_attempting`) and keeps the registry's exit code — so the two surfaces no longer
/// differ on WHAT is shown, only on the exit code a REPL does not have. It is here that the rule is
/// cheapest to obey and hardest to argue against: the mode is ALREADY IN HAND — the node pushed it
/// — so withholding it because a second request failed would be discarding an answer the operator
/// has, at exactly the moment (an old node, a half-open link) they most want to know whether
/// trading is halted.
fn run_status(session: &Session<'_>) {
    with_snapshot(session, print_state);
    let Some((key, _origin)) = session.keys.observe() else {
        // Unreachable in practice: `with_snapshot` above already said reads are disabled, and it
        // says it better. Kept so the registry half never silently prints nothing.
        return;
    };
    // ⚠ `_with_features` rather than `strategy_status`, and the same call underneath: the PRODUCT
    // column can only be rendered honestly by a reader that knows whether the node advertised
    // `FEATURE_MOUNT_CLASS`. An absent class means "this node is too old to say" or "this mount is
    // not migrated yet", and only the capability tells them apart.
    match vike_tradehub_client::strategy_status_with_features(session.node.as_str(), key.as_bytes())
    {
        Ok((status, features)) => {
            let knows_class =
                features.iter().any(|f| f == vike_tradehub_client::proto::FEATURE_MOUNT_CLASS);
            for line in crate::cmd::trade::status::registry_lines(&status, knows_class) {
                println!("{line}");
            }
        }
        // Printed, not returned: a REPL has no exit code, and the mode above already landed.
        Err(e) => {
            for line in crate::cmd::trade::status::failure_lines(&session.node, &e) {
                println!("{line}");
            }
        }
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
    let snap = wait_for_first_frame(observe);
    if !observe.is_connected() {
        println!("[warning] observe connection is down — showing the last snapshot seen");
    }
    f(&snap);
}

/// Wait up to [`FIRST_FRAME_WAIT`] for `observe`'s next REAL (`seq != 0`) pushed frame, polling its
/// latest snapshot. A freshly-subscribed connection returns the empty placeholder (`seq 0`) until
/// the node pushes its first coalesced frame. Returns whatever is in hand once the deadline passes
/// — the placeholder if the node never got there in time.
///
/// Shared by [`with_snapshot`] (this session's own reads, over a session-lifetime connection) and
/// by [`connect_observe`] (a one-shot group verb's fresh connection, e.g. `crate::cmd::trade::order`
/// 's `ls`) — one "wait for real data" implementation rather than two.
pub(crate) fn wait_for_first_frame(observe: &RemoteCoreHandle) -> std::sync::Arc<WireSnapshot> {
    let deadline = Instant::now() + FIRST_FRAME_WAIT;
    let mut snap = observe.snapshot();
    while snap.seq == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        snap = observe.snapshot();
    }
    snap
}

/// Connect the OBSERVE half to `node` with `key` and wait for the first real frame — see
/// [`wait_for_first_frame`]. The one-shot twin of this session's own connect-then-defer-the-wait
/// sequence in [`run`], for a group verb that holds no session to defer the wait to. Returns the
/// connected handle (drop it to close the connection) alongside the snapshot in hand once the wait
/// ended.
pub(crate) fn connect_observe(
    node: &str,
    key: &[u8],
) -> std::io::Result<(RemoteCoreHandle, std::sync::Arc<WireSnapshot>)> {
    let handle = RemoteCoreHandle::connect(node, key)?;
    let snap = wait_for_first_frame(&handle);
    Ok((handle, snap))
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
fn run_set_mode_once(
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
fn mode_verb(state: WireTradingState) -> &'static str {
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
fn mode_usage(verb: &str) -> String {
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

/// The parsed one-shot mode command line.
#[derive(Debug, PartialEq, Eq)]
struct ModeArgs {
    /// `--node`: the node's control address. `None` is a usage error at the call site rather than
    /// here, so `--help` short-circuits before it is ever missed.
    node: Option<String>,
    /// `--yes`/`-y`: skip the confirm prompt. OFF unless asked for.
    yes: bool,
    /// `--reason`: the operator rationale sent beside the command for the node's audit trail.
    reason: Option<String>,
}

/// `--node` / `--yes` / `--reason` for the one-shot mode verbs. PURE — unit-tested below.
fn parse_mode_args(args: impl Iterator<Item = String>) -> Result<ModeArgs, String> {
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
fn confirm_stdin() -> bool {
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
fn is_yes(answer: &str) -> bool {
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
fn run_set_setting(
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

// ⚠ `fn typed_key_confirm(rl, key)` stood here — the SECOND prompt a policy `set-setting` asked
// after the y/N ("retype the key to confirm"), which `--yes` could not answer, so a policy write
// was the one REPL line a script could not complete. DELETED with the ceremony it served
// (`docs/decisions/0086` point 7: *"confirmation over confirmation … a nightmare"*); the node had
// already stopped reading the wire field it filled. Before it, `fn is_policy_file(file: &str)`
// decided which lines were asked, from the FILE NAME an operator typed, until that decision moved
// onto the key's section word. Both names stay in this comment because records and specs written
// while they shipped cite them — the repair to a citation of deleted code is a tombstone, never a
// deleted citation.

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
    Some(settings_dir?.join(vike_model::paths::state_path::STATE_SUBDIR).join(HISTORY_FILE))
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
        // is a whole-token match, so it collides with nothing — including the REMOVED `state`,
        // whose migration hint [`parse_line`] still answers by name.
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
/// **`--account` names WHICH ACCOUNT of the venue the mount trades and reads**, and the three
/// states are distinct: omitted names none, `DEFAULT` names the venue's unlabelled account
/// deliberately, a label names that account. On a venue this node runs TWO engines of, omitting it
/// is REFUSED by the node rather than resolved to the default — which is the whole point, since the
/// default account's route key IS the bare venue, so the silent answer would look correct. The
/// refusal names the spellings that core answers to. Requires the node to advertise
/// `FEATURE_MOUNT_ACCOUNT`; against an older node a NAMED account is refused here, unsent.
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
                         <path-on-the-node>) [--account <LABEL|DEFAULT>] [--id <mount-id>] \
                         [--params <json to end of line>]";

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

    let (mut name, mut rhai, mut id, mut account): (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = (None, None, None, None);
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
            "--name" | "--rhai" | "--id" | "--account" => {
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
                    "--account" => &mut account,
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

    // Refused HERE, in the REPL's own vocabulary, rather than a round trip away — but read with
    // `parse_wire_account`, the one authority on the grammar, so this edge adds a message and not a
    // second set of rules. ⚠ It admits `DEFAULT`, which a `policy.accounts` row refuses: on the
    // wire that spelling is how an operator says "the unlabelled account, deliberately" as
    // distinct from saying nothing, and at two engines of one venue those are different answers.
    if let Some(a) = &account
        && let Err(e) = vike_model::accounts::account_keys::parse_wire_account(a)
    {
        return Err(format!(
            "mount: --account {a:?} — {e}. Drop the flag to name no account, or pass DEFAULT to \
             name the venue's unlabelled account deliberately\n{USAGE}"
        ));
    }

    Ok(Verb::MountStrategy {
        venue: positional[0].to_string(),
        account,
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

/// `set-setting <full.dotted.key> <value to end of line>` — write ONE row of the node's settings
/// database, named by its key ([`Verb::SetSetting`]; `docs/decisions/0086`). The same two
/// positionals `vike-cli config set` takes, for the same reason: the key's first segment already
/// names its section.
///
/// The value runs to END OF LINE and is taken VERBATIM. Both halves of that are deliberate: a TOML
/// value can contain spaces (`["a", "b"]`, a prose string), and unlike `--reason` it is NOT
/// unquoted, because quotes are meaningful to the node's TOML parse — stripping them would turn the
/// string `"250"` into the integer `250`, silently changing the type of the key being set.
///
/// ⚠ **The retired `<file>` token is refused BY NAME.** The grammar was
/// `set-setting <file> <key> <value>` until a write became one row named by its key; a line still
/// spelled that way would otherwise parse its FILE NAME as the key and the real key as the start of
/// the value. A first token that is one of the four settings sections' file names, or a bare
/// section word (`vike_config::SettingsFile::parse`), is that retired token: no settings key is
/// either one (a key names a field INSIDE a section), so the refusal can never catch a real write.
///
/// Nothing here validates the KEY. The node holds the loader and refuses an unknown key with its
/// own message; a second copy of that rule on this side would be one more thing to keep in step,
/// and it would still not be the one that decides. What goes on the wire — `file` derived from the
/// key, `confirm` empty — is [`Verb::to_wire_command`]'s to decide.
fn parse_set_setting(rest: &str) -> Result<Verb, String> {
    const USAGE: &str = "usage: set-setting <full.dotted.key> <value to end of line>";
    let (key, value) = take_token(rest).ok_or(USAGE)?;
    if vike_config::SettingsFile::parse(key).is_some() {
        return Err(format!(
            "set-setting takes no file any more — a write is one row named by its key, and the \
             key's first segment is its section (docs/decisions/0086). Drop {key:?}: \
             set-setting <full.dotted.key> <value to end of line>, e.g. `set-setting \
             policy.max_notional_per_order 250`\n{USAGE}"
        ));
    }
    // Trailing whitespace is stripped and surrounding quotes are NOT (see the doc above) — the two
    // are different acts: one removes what the terminal added, the other would change the type.
    let value = value.trim_end();
    if value.is_empty() {
        return Err(format!(
            "set-setting: no value — a key set to nothing is not a write anybody meant\n{USAGE}"
        ));
    }
    Ok(Verb::SetSetting { key: key.to_string(), value: value.to_string() })
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
            // ⚠ `account: None` — this REPL grammar takes no book selector; task 7 of the
            // trade-CLI-plane widened `Verb::Flatten` with the field for the one-shot
            // `crate::cmd::trade::position`'s `flatten`, which DOES take one. Lifting the REPL onto
            // the same selector grammar is future work, not a gap this line's arity covers.
            Ok(Verb::Flatten {
                venue: rest[0].to_string(),
                symbol: rest[1].to_string(),
                account: None,
            })
        }
        "market-exit" | "panic" => {
            if rest.len() > 1 {
                return Err("usage: market-exit [venue]".to_string());
            }
            // Same note as `flatten` above.
            Ok(Verb::MarketExit { venue: rest.first().map(|s| s.to_string()), account: None })
        }
        "mass-cancel" => {
            if rest.len() > 2 {
                return Err("usage: mass-cancel [venue] [symbol]".to_string());
            }
            // Same note as `flatten` above.
            Ok(Verb::MassCancel {
                venue: rest.first().map(|s| s.to_string()),
                symbol: rest.get(1).map(|s| s.to_string()),
                account: None,
            })
        }
        // ⚠ THREE WORDS, NOT ONE WITH AN ARGUMENT. `state` used to be this arm — no token meant
        // READ, a token meant WRITE — so `state halted` halted a live daemon by adding a word to a
        // read. Ruling 17 removed it: `status` reads (the mode AND the mount registry, in one
        // output), `halt` and `resume` each write and each says so in its own name. Every one of
        // them REFUSES a trailing token, which is what keeps the old spelling from being half-alive:
        // `status halted` is a usage error naming the two verbs that write, never a halt.
        "status" => {
            if rest.is_empty() {
                Ok(Verb::Status)
            } else {
                Err("usage: status  (it takes no argument — `halt` and `resume` are the verbs \
                     that CHANGE the mode)"
                    .to_string())
            }
        }
        "halt" => {
            if rest.is_empty() {
                Ok(Verb::SetState(WireTradingState::Halted))
            } else {
                Err("usage: halt  (it takes no argument)".to_string())
            }
        }
        "resume" => {
            if rest.is_empty() {
                Ok(Verb::SetState(WireTradingState::Active))
            } else {
                Err("usage: resume  (it takes no argument)".to_string())
            }
        }
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
        // ⚠ THE MIGRATION HINT, and it is deliberately an `Err` rather than an alias. `state` is
        // removed, not renamed — the no-shims rule applied to a grammar, and a shim that still
        // worked would keep alive the exact line (`state halted`) ruling 17 exists to delete. But
        // this prompt is where the removed word gets typed under pressure, off a runbook somebody
        // wrote in 2026, at the moment they most need the daemon to stop: an operator who reads
        // `unknown command: "state"` and has to go and find a page has been made slower by a safety
        // fix. So the refusal REFUSES and then names both replacements, in the order they are
        // needed. It sits here rather than in the arms above because there is no `state` arm any
        // more and there must not be one — this is the unknown-verb catch-all, answering one word
        // by name. [`STATE_MIGRATION_HINT`] is the sentence; `run` prints the same one for the
        // one-shot spelling, so the two surfaces cannot come to say different things.
        "state" => Err(STATE_MIGRATION_HINT.to_string()),
        other => Err(format!("unknown command: {other:?}")),
    }
}

/// What an operator who typed the REMOVED `state` verb is told — at the REPL prompt
/// ([`parse_line`]) and at the one-shot command line ([`run`]) alike, one sentence so the two
/// cannot drift. It names the READ first and the two WRITES after it: the commonest reason to type
/// the old word was to look, and the reader is mid-incident.
const STATE_MIGRATION_HINT: &str = "`state` was REMOVED (it both read and wrote the trading mode, \
                                    one token apart). Use `status` to READ the mode, `halt` to stop \
                                    trading, `resume` to start it again.";

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
        account: None,
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

// ⚠ `parse_state` — `active`/`reducing`/`halted` → the wire trading state — was DELETED here by
// ruling 17. It existed only to read the argument of the `state` verb, and that argument is the
// defect: a word that turned a read into a halt. `halt` and `resume` name their own state, so
// nothing parses one from operator text on this surface any more. The MCP tool `set_trading_state`
// keeps its own three-way match (`crate::cmd::verbs`'s `verb_from_tool_args`) — an agent naming a
// state in a JSON field is not a human adding a token to a read — and it is where `Reducing` is
// still spelled.

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
        // No REPL verb mints a bracket (the desktop's TP/SL ticket is its one producer), but the
        // preview renderer stays total for a command another caller built. `account=-` because a
        // bracket names none: the node takes it only on a venue's one default account.
        WireCommand::Bracket(b) => format!(
            "BRACKET {} {} {} qty={} entry={} sl={} tp={} account=-",
            side_word(b.side),
            b.venue,
            b.symbol,
            verbs::fmt_num(b.qty),
            b.entry_price.map(verbs::fmt_num).unwrap_or_else(|| "market".to_string()),
            verbs::fmt_num(b.stop_loss),
            verbs::fmt_num(b.take_profit),
        ),
        WireCommand::Cancel(coid) => format!("CANCEL {coid}"),
        WireCommand::Modify { client_order_id, new_qty, new_price } => format!(
            "MODIFY {client_order_id} qty={} price={}",
            new_qty.map(verbs::fmt_num).unwrap_or_else(|| "-".to_string()),
            new_price.map(verbs::fmt_num).unwrap_or_else(|| "-".to_string()),
        ),
        WireCommand::MassCancel { venue, symbol, .. } => {
            format!("MASS-CANCEL venue={} symbol={}", opt_str(venue), opt_str(symbol))
        }
        WireCommand::Flatten { venue, symbol, .. } => format!("FLATTEN {venue} {symbol}"),
        WireCommand::MarketExit { venue, .. } => format!("MARKET-EXIT venue={}", opt_str(venue)),
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
            account,
            symbol,
            interval,
            controller_id,
            name,
            rhai,
            params,
        } => {
            // ⚠ The ACCOUNT is rendered for the reason stated just above about the knobs, only
            // sharper: a mount is not one order, it is every order that strategy will ever place,
            // and WHICH BOOK it places them on is the one thing this line cannot leave out. Absent
            // renders as `-` rather than as `default` — the operator named nothing, and printing a
            // book name they did not choose would be a reassurance the node has not agreed to (on
            // a two-engine venue it will refuse this mount outright).
            format!(
                "MOUNT-STRATEGY {venue} account={} {symbol} {interval} id={} source={} {params}",
                account.as_deref().unwrap_or("-"),
                controller_id.as_deref().unwrap_or("-"),
                name.as_deref().or(rhai.as_deref()).unwrap_or("-"),
            )
        }
        WireCommand::UnmountStrategy { controller_id } => {
            format!("UNMOUNT-STRATEGY {controller_id}")
        }
        // The REQ-7 settings write — spelled by [`parse_set_setting`]. Show the ASSIGNMENT, the one
        // thing it is: a row named by its key. ⚠ The two file-era wire fields are not rendered:
        // `file` is derived from the key (so printing it repeats the key's first word), and a
        // `confirm` — which only an older client fills — is ignored by the node since
        // `docs/decisions/0086` point 7, so a badge for it would advertise a guard that does not
        // exist. The `old → new` beside this line needs the node's current value, so [`run_write`]
        // prints it from a node read.
        WireCommand::SetSetting { key, value, .. } => format!("SET-SETTING {key} = {value}"),
    }
}

/// `pub(crate)`: shared with `crate::cmd::trade::render`'s order-row projection, so the two
/// renderers cannot come to disagree about what a side prints as.
pub(crate) fn side_word(side: i32) -> &'static str {
    if side >= 0 { "buy" } else { "sell" }
}

/// `pub(crate)`: shared with `crate::cmd::trade::render`, for the same reason as [`side_word`].
pub(crate) fn opt_num(v: &Option<f64>) -> String {
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

/// The MODE block: the trading state, prefixed with this frame's staleness stamp, plus the fault
/// line when the core is in safe state.
///
/// The WORDING is [`crate::cmd::trade::status::mode_lines`]', not this file's — three surfaces render
/// the mode (`status` here, `snapshot` here, and the one-shot `vike-cli trade status`) and they may
/// not describe a kill switch differently. What stays here is the `[seq N]` prefix, which is a REPL
/// fact: this snapshot arrived by PUSH and may be old, while the one-shot's is a fresh round trip.
fn print_state(snap: &WireSnapshot) {
    let mut lines =
        crate::cmd::trade::status::mode_lines(snap.trading_state, snap.fault.as_deref())
            .into_iter();
    if let Some(first) = lines.next() {
        println!("{} {first}", stamp(snap));
    }
    for line in lines {
        println!("{line}");
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
    // ⚠ Each is a WORD, and the help says what it does rather than what it sets: `state
    // <active|reducing|halted>` used to live here, and an argument that turns a read into a halt is
    // the shape ruling 17 removed. The halt line states the covered-reduce exemption, because an
    // operator who reads "halted" as "I am now trapped" un-halts to get out — restarting the
    // strategy that got them there. `docs/ops/kill-switches.md` argues that at length.
    println!(
        "    halt                         (mode -> Halted: no order that OPENS or ADDS risk; \
         position-covered reduces still pass, so market-exit/flatten still work)"
    );
    println!("    resume                       (mode -> Active)");
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
    println!("    set-setting <full.dotted.key> <value to end of line>");
    // Say what the preview shows and that nothing else is asked: a scripted session reading this
    // screen must be able to tell that `--yes` covers the settings write like every other write.
    println!(
        "      (one row of the node's settings, named by its key and validated by the node's own \
         loader; the preview shows old → new, read off the node, and the y/N is the only question)"
    );
    println!("  any WRITE line may end with:");
    println!("    --reason <text to end of line>   recorded in the node's audit trail");
    println!("  READ:");
    println!("    orders [symbol]");
    println!("    positions [venue]");
    println!("    equity");
    println!("    recent [N]");
    println!(
        "    status                       (the trading mode AND one row per mounted strategy)"
    );
    println!("    snapshot");
    println!("  meta:  help    quit|exit  (or Ctrl-D)");
    // The one-shot half of ruling 16: the same three words, outside the prompt. Stated in the REPL's
    // own help because an operator who has found the prompt is exactly the one who then wants to
    // put a halt in a runbook or a unit file.
    println!(
        "  the same three words run one-shot, no REPL: vike-cli trade <status|halt|resume> \
         --node <host:port>"
    );
}

#[path = "trade_tests.rs"]
#[cfg(test)]
mod trade_tests;
