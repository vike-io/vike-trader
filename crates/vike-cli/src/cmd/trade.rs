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
//! environment first, then the settings database's `node_key` table — the same node-key store
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
//! order at all**. It is now filled from the session's `vike_model::orders::client_order_id::ClientOrderIdGenerator`
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
//! needs the mount's current params to patch against (`WireMountRow::typed_params` is that structured
//! read now, and `WireMountRow::mount_id` names the one mount to address), and no such
//! read-modify-write verb is built. Spelling it as "send a whole params object" would let an
//! operator who meant to change one knob silently reset the other sixteen to a strategy's
//! defaults. It is a separate design track, not an oversight.
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

#[cfg(test)]
use serde_json::json;
use vike_model::orders::client_order_id::ClientOrderIdGenerator;
#[cfg(doc)]
use vike_tradehub_client::wire::WireCommand;
#[cfg(test)]
use vike_tradehub_client::wire::{WireCommand, WireOrderRequest};
use vike_tradehub_client::wire::{WireSnapshot, WireTradingState};
use vike_tradehub_client::{RemoteControlHandle, RemoteCoreHandle};

use crate::cmd::args::{self, Flags};
use crate::cmd::nodekeys::{self, NodeKeyring};
use crate::cmd::verbs;
#[cfg(doc)]
use crate::cmd::verbs::Verb;
#[cfg(test)]
use crate::cmd::verbs::Verb;
use crate::exit::Exit;

use self::describe::{describe_command, opt_num, side_word};
use self::grammar::unquote;
use self::grammar_orders::side_from_word;
use self::help::print_verb_help;
use self::repl::{history_file, repl};
use self::write::{confirm_stdin, run_set_mode_once};

#[cfg(test)]
use self::describe::{orders_table, stamp, trunc_mid};
#[cfg(test)]
use self::grammar::{PARAMS_FLAG, parse_line, parse_repl_line, split_reason, split_tail};
#[cfg(test)]
use self::write::{is_yes, mode_usage, mode_verb, parse_mode_args};

#[cfg(doc)]
use self::grammar::{
    parse_lifecycle, parse_line, parse_repl_line, parse_set_setting, split_reason,
};
#[cfg(doc)]
use self::grammar_orders::parse_submit;
#[cfg(doc)]
use self::write::run_set_setting;

mod oneshot;
// The read-only node question `vike-cli trade status` answers.
mod order;
mod plane;
mod position;
mod render;
mod selector;
pub mod status;
mod strategy;

mod describe;
mod grammar;
mod grammar_orders;
mod help;
mod observe;
mod repl;
mod write;

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

/// How long a display read waits for a frame the node pushed before it answers whatever is in hand
/// — see [`wait_for_first_frame`].
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

/// THE DISPLAY READ'S WAIT: until `observe` holds a frame the NODE pushed ([`is_node_frame`]), the
/// handle dies, or [`FIRST_FRAME_WAIT`] passes — then whatever is in hand, which is the client's
/// own placeholder only if no node frame arrived in time. The node hands a new subscriber its
/// current frame at once, so against a reachable node this ends within a round trip. `alive` is
/// read BEFORE the cell, so a death seen here is ordered after every frame the receive thread
/// stored: the frame a caller then judges is that handle's last.
///
/// ONE wait for every read that only DISPLAYS a subscribed frame, so no two surfaces can disagree
/// about when a read may answer: [`with_snapshot`] (the REPL's reads, over a session-lifetime
/// connection), [`connect_observe`] (the one-shot `trade order ls` and `trade position ls`) and
/// the `mcp` server's `node_snapshot` tool and `vike://node/snapshot` resource (`Server::node_frame`
/// in `crates/vike-cli/src/cmd/mcp/node_reads.rs`).
///
/// ⚠ **Not for a read that JUDGES a write.** A frame this answers may be the node's pre-fold
/// placeholder ([`is_pre_fold`]): `venues: []` and `accounts_epoch: 0`, neither a reading. The
/// `mcp` order path — its venue gate and the preview's epoch stamp — keeps its own wait for a FOLD
/// (`wait_for_a_fold`, beside `Server::node_frame`, whose doc says why the latency is paid there on
/// purpose).
/// The REPL's and the one-shot verbs' write paths read no snapshot at all: [`run_write`] and
/// `crate::cmd::trade::oneshot`'s `run_write` preview from the command and the node's own verdict.
///
/// ⚠ **`seq != 0` was this wait's condition until 2026-10-04**, and for a display read it was a
/// latency defect, the same one `node_snapshot` had: a node that has not folded never leaves
/// `seq: 0`, so against an idle node EVERY REPL read — not just a session's first — and every
/// one-shot `ls` sat out the whole deadline and then printed the frame that had been in hand since
/// the first round trip.
pub(crate) fn wait_for_first_frame(observe: &RemoteCoreHandle) -> std::sync::Arc<WireSnapshot> {
    let deadline = Instant::now() + FIRST_FRAME_WAIT;
    loop {
        let alive = observe.is_connected();
        let snap = observe.snapshot();
        if is_node_frame(&snap) || !alive || Instant::now() >= deadline {
            return snap;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Whether `snap` is a frame the NODE pushed, rather than the `WireSnapshot::empty()` placeholder a
/// freshly opened [`RemoteCoreHandle`] holds until the node's first frame lands — the condition
/// [`wait_for_first_frame`] ends on.
///
/// Two pieces of evidence, each SUFFICIENT and neither NECESSARY, because the client's placeholder
/// carries neither (`identity: None`, `seq: 0`):
///
/// - **`identity`** — the node's publisher stamps its identity block into EVERY frame it pushes,
///   including the one it publishes before its first fold, and the shipped daemon always has one
///   (`crates/vike-tradehub/src/node.rs`'s `spawn_with_mounts` call). This is the arm that
///   answers a node that has not folded.
/// - **`seq > 0`** — only a fold moves it. This is the arm that answers a node that stamps no
///   identity block (one that predates it) once that node has folded.
///
/// ⚠ **A frame that satisfies this is NOT necessarily a reading.** The stamped `seq: 0` frame is the
/// node's OWN placeholder — `vike_exec::CoreSnapshot::empty`, "published before the first real
/// build" — carrying `venues: []`, zero balance and equity, and `accounts_epoch: 0`, each of which
/// vike-core means as "has not said yet". So every display read MARKS it ([`is_pre_fold`]) instead
/// of presenting it as the node's state, and the `mcp` order path does not use this condition at all.
///
/// ⚠ **What neither arm can see: a node that stamps no identity and has never folded.** Its one
/// frame carries neither piece of evidence — on the two fields read here it is the client's
/// placeholder — so against it a read still runs to its deadline, which is what it always did.
pub(crate) fn is_node_frame(snap: &WireSnapshot) -> bool {
    snap.identity.is_some() || snap.seq > 0
}

/// Whether `snap` carries nothing BUILT yet: `seq: 0`, which only two frames carry — the node's own
/// pre-fold placeholder (stamped with its identity) and this client's own `WireSnapshot::empty()`
/// (no identity). [`is_node_frame`] tells the two apart. A display read MARKS such a frame rather
/// than rendering its zeros and empty lists as the node's state — the `mcp` read tools as
/// `pre_fold` (`node_read_answer` in `crates/vike-cli/src/cmd/mcp/node_reads.rs`), the REPL and the
/// one-shot `ls` verbs with [`pre_fold_line`] and the [`stamp`] on every table header.
pub(crate) fn is_pre_fold(snap: &WireSnapshot) -> bool {
    snap.seq == 0
}

/// The line a DISPLAY read prints ahead of a frame that carries nothing built ([`is_pre_fold`]), or
/// `None` for a built one. The REPL prints it on stdout above the render ([`with_snapshot`]); the
/// one-shot `ls` verbs print it on STDERR, so a `--json` stdout stays one bare document.
///
/// TWO wordings, because they are two different facts and an operator acts on them differently:
/// with an identity the node is up and has published its pre-fold placeholder (it has folded
/// nothing — an idle paper node stays here until its first order); without one, nothing
/// recognisable as the node's arrived in time, and what is in hand is this client's own
/// placeholder.
pub(crate) fn pre_fold_line(snap: &WireSnapshot) -> Option<&'static str> {
    if !is_pre_fold(snap) {
        return None;
    }
    Some(if is_node_frame(snap) {
        "[pre-fold] the node is up and has published only its pre-fold placeholder (seq 0, \
         stamped with its identity): it has built nothing yet, so every balance, equity figure \
         and list below is a placeholder, not a reading — an empty list does NOT mean there is \
         nothing. Read again once the node has folded."
    } else {
        "[pre-fold] no frame from the node arrived on this connection in time (seq 0, no \
         identity), so what follows is this client's OWN empty placeholder, not the node's \
         state — an empty list does NOT mean there is nothing. (A node too old to stamp an \
         identity looks the same until its first fold.) Read again."
    })
}

/// Connect the OBSERVE half to `node` with `key` and wait for a frame the node pushed — see
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

// ⚠ `fn typed_key_confirm(rl, key)` stood here — the SECOND prompt a policy `set-setting` asked
// after the y/N ("retype the key to confirm"), which `--yes` could not answer, so a policy write
// was the one REPL line a script could not complete. DELETED with the ceremony it served
// (`docs/decisions/0086` point 7: *"confirmation over confirmation … a nightmare"*); the node had
// already stopped reading the wire field it filled. Before it, `fn is_policy_file(file: &str)`
// decided which lines were asked, from the FILE NAME an operator typed, until that decision moved
// onto the key's section word. Both names stay in this comment because records and specs written
// while they shipped cite them — the repair to a citation of deleted code is a tombstone, never a
// deleted citation.

/// What an operator who typed the REMOVED `state` verb is told — at the REPL prompt
/// ([`parse_line`]) and at the one-shot command line ([`run`]) alike, one sentence so the two
/// cannot drift. It names the READ first and the two WRITES after it: the commonest reason to type
/// the old word was to look, and the reader is mid-incident.
const STATE_MIGRATION_HINT: &str = "`state` was REMOVED (it both read and wrote the trading mode, \
                                    one token apart). Use `status` to READ the mode, `halt` to stop \
                                    trading, `resume` to start it again.";

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

#[path = "trade/tests.rs"]
#[cfg(test)]
mod trade_tests;
