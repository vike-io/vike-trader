//! `vike-telegram` — the opt-in Telegram CONTROL channel: a remote write surface reachable from a
//! phone, with **full write** behind a mandatory preview/confirm contract, run as its own process
//! (`vike-backend telegram`, `deploy/vike-telegram.service`) and reaching the trading node ONLY as a
//! client of its control wire, like `vike-cli` and `vike-mcp` (decision 0256).
//!
//! ## The accepted risk, stated plainly, once
//!
//! With this channel enabled, **possession of the bot token plus membership in an allowlisted chat
//! is sufficient to place real orders.** Telegram is a third party sitting in an order-origination
//! path. The node's `ControlLimits` (notional + rate) and the core `RiskGate` still bound the blast
//! radius, and every gate below applies, but that residual does not go away — it was raised, and the
//! full-write scope was chosen deliberately.
//!
//! ## Layout: the security-critical files are SEPARATE on purpose
//!
//! The jobs this crate does are not equally dangerous, and interleaving them in one file made that
//! impossible to see. Three files are where a bug means **UNAUTHORIZED ORDER PLACEMENT**:
//!
//! | File | Job | If it is wrong |
//! |---|---|---|
//! | **`config.rs`** ⚠ | the two control-flag gates, the bot token, the chat allowlist | a stranger commands the node |
//! | **`confirm.rs`** ⚠ | token single-use / expiry / command+chat binding | a confirmation fires twice, or fires something it never previewed |
//! | **`ledger.rs`** ⚠ | at-most-once across a restart | a restart replays an order |
//! | `parse.rs` | the chat-string grammar → [`Instruction`] | a command is misread — caught by the preview an operator reads before confirming |
//! | `tick.rs` | ONE poll → dispatch pass | the ORDER of dedupe → mark → allowlist → dispatch is itself security-critical |
//! | `render.rs` | read verbs printed off the node's frame | an ugly message |
//! | `failure.rs` | permanent-vs-transient `getUpdates` failure + the retry schedule | a third party is hammered ~2x/second forever, or a working channel stops |
//! | `deps.rs` | the `ureq` long-poll and the node's control wire | (it holds the token in a URL and the hub keys — hence no `Debug`) |
//! | `telegram_cli.rs` | the program: boot, the reads of the rows, the start, the stop | the channel starts when a gate says it must not |
//!
//! That table is the point of the split: "this change does not touch the security-critical code" is
//! a property a reviewer reads off the CHANGED-FILE LIST, not one they have to re-derive.
//!
//! ## Placement: its own process, a CLIENT of the node
//!
//! It lived inside `vike-tradehub` behind a `telegram` feature, holding the core's command sink and
//! snapshot cell and calling the door in-process with its own preview rules and its own copy of the
//! door's reads. It now holds none of that: the tradehub links no Telegram code, and this process
//! asks the node over the wire the way every other remote surface does (`deps.rs`'s module doc says
//! which call each verb is). So the door, its preview and its words are the ones `vike-cli` and
//! `vike-mcp` reach, **by construction, not by review**, and a step added to the door reaches this
//! surface with no edit here. A restart of this process touches no engine, and the tradehub runs
//! with it stopped.
//!
//! `vike-alerting` already delivers OUTBOUND Telegram messages and owns the Bot API's URL and body
//! shapes, which this crate reuses; it stays a leaf with no `vike-*` dependency, so lowering a chat
//! message into a command (`vike-client`'s vocabulary) lives here, at layer 35 beside the CLI.
//!
//! ## Gates — ALL FOUR required simultaneously, plus the node's own
//!
//! | Gate | Where | Semantics |
//! |---|---|---|
//! | `flags.tradehub_control` | settings flag (a row) | resolved `true` ⇒ the exact string `"1"` |
//! | `flags.telegram_control` | settings flag (a row) | resolved `true` ⇒ the exact string `"1"` |
//! | `VIKE_TELEGRAM_BOT_TOKEN` | credential store | absent/blank ⇒ nothing constructed |
//! | `VIKE_TELEGRAM_ALLOWED_CHAT_IDS` | credential store | comma-separated; EMPTY ⇒ disabled |
//!
//! The two flags are read from the settings database by this process's boot and reach this module as
//! parameters: `crates/vike-telegram/src/telegram_cli.rs`'s `run` passes a resolved `true` as
//! `Some("1")` and `false` as `None`, which [`control_gates_open`]'s exact-`"1"` grammar already
//! reads as closed. The node then holds its own gates on top: its control server answers only a
//! `Scope::Write` handshake under the tradehub's control key (`flags.tradehub_control` on THAT
//! process), which this process reads from the `hub_key` table as the CLI does. It reads no venue
//! credential at all.
//!
//! …plus ONE optional fifth, which can only ever NARROW:
//!
//! | `VIKE_TELEGRAM_ALLOWED_USER_IDS` | credential store | comma-separated; UNSET ⇒ chat-only |
//!
//! Absent any one of the four ⇒ **no poll, no token in memory, no network call**. [`arm`] enforces
//! that ordering literally: the two flags are checked by the pure [`control_gates_open`] FIRST, and
//! the credential-store loader is passed as a FUNCTION, so on the closed path the store is never even
//! read and the bot token never enters this process's address space.
//!
//! An **unlisted `chat_id` is ignored and logged, never answered** — no reply, not even an error. A
//! bot that answers unknown chats is an oracle confirming a trading node exists behind it.
//!
//! ## Authorization is per-CHAT; ATTRIBUTION is per-USER
//!
//! ⚠ The gate above asks one question — *is this chat allowlisted?* — and a chat is not a person.
//! An allowlisted `-100…` supergroup authorizes **every member**, including everyone anyone later
//! adds, with no config change and nothing to review. That is the documented policy and it stays
//! the DEFAULT, because the ordinary deployment is a single operator's DM where a chat IS a person.
//!
//! Two things narrow the gap without changing who is authorized by default:
//!
//! 1. **Attribution, always.** [`parse_updates`] reads `message.from.id` (and the `@handle`), and
//!    [`confirm_reason`] / [`confirmed_by`] put both the instructing and the confirming actor into
//!    the audit rationale the node records beside the command.
//! 2. **An optional per-user allowlist.** `VIKE_TELEGRAM_ALLOWED_USER_IDS`
//!    ([`TelegramConfig::allows_user`]) is ANDed with the chat check when configured, and unset —
//!    the default — means chat-only, exactly as before.
//!
//! The confirm TOKEN stays chat-bound, not user-bound (see [`Pending::from_id`]): binding it would
//! stop an operator confirming a colleague's preview, which is a change to who may authorize an
//! order and belongs to the opt-in above, not to a default.
//!
//! ## Confirm contract — mirrors the MCP write-tool surface
//!
//! 1. A write instruction is answered with a **preview**: the resolved
//!    [`CommandWire`](vike_tradehub_client::wire::CommandWire), the node's own dry-run verdict
//!    (`Request::Preview`), and a single-use token. **Nothing is sent.**
//! 2. Only `/confirm <token>`, within [`CONFIRM_WINDOW_MS`] (60 s), executes.
//! 3. The token is single-use ([`PendingConfirms::take`] REMOVES it before the command is handed to
//!    the node) and bound to the exact command it previewed AND to the chat that asked for it. A
//!    token that has fired, expired, or belongs to another chat is refused.
//!
//! That contract is also what makes a first-run backlog harmless: pending confirmations live only in
//! memory, so a day-old `/confirm` replayed out of Telegram's queue after a restart finds no pending
//! entry and executes nothing, while a replayed `/submit` produces only another preview.
//!
//! ## At-most-once
//!
//! Telegram's `update_id` is monotonic. [`UpdateLedger`] persists the high-water mark (append-only
//! lines, max-on-load) and every `getUpdates` passes `offset = last + 1`, which is also how Telegram
//! is ACKed. An update is marked **BEFORE** it is acted on, deliberately: a process that dies
//! mid-command must lose that command, never place it twice.
//!
//! The file is `<project>/settings/state/telegram_updates.ledger` — the one root every
//! program-written file resolves to (`crates/vike-paths/src/state_path.rs`'s `project_state_dir`),
//! handed in by the program as a path, and the same file the tradehub wrote while this channel was
//! one of its threads, so the mark carries over the move.
//!
//! ⚠ **A ledger write that cannot land is an ERROR, never a shrug.** Silently accepting a command you
//! cannot record is the worst available option on an order-origination path, so it is not available:
//!
//! * **arming** — [`arm`] opens the ledger, which proves it readable and appendable, and returns
//!   `None` with a `tracing::error!` if it is not: the channel does not exist.
//! * **running** — [`UpdateLedger::mark`] returns `Err` and [`poll_once`] drops that update
//!   undispatched (reported as `TickReport::unrecorded`), for the case where the path stopped being
//!   writable under a running process.
//!
//! ## Transport
//!
//! `getUpdates` long-poll — an outbound HTTPS GET, no inbound listener. A pure tick function behind a
//! `*Deps` trait seam so the logic tests with NO network, on its OWN `ureq::Agent`
//! (`vike_alerting::no_redirect_agent` over [`AGENT_TIMEOUT`]): the webhook transport's 10 s global
//! timeout is shorter than one long-poll.
//!
//! The bot token is embedded in the API base URL, so [`ProdTelegramDependencies`] has NO `Debug`, never
//! interpolates the URL or a transport error into any message, and [`TelegramConfig`]'s `Debug`
//! redacts the token.
//!
//! ⚠ **A failing endpoint is not retried blindly.** `getUpdates` fails INSTANTLY when the bot token
//! is wrong (there is no long-poll to wait through), so a loop that answers every failure with
//! [`POLL_GAP`] and another attempt settles at ~2 requests/second against a third party, forever —
//! measured on a clean install with a mistyped token. [`PollError`] therefore separates a rejected
//! CREDENTIAL (401/403/404 — retrying can never help, so the channel STOPS with one `error!`) from
//! everything else (backed off to a [`BACKOFF_MAX`] ceiling, announcements throttled). The policy,
//! its measured before/after volume and the reasoning for each status live in
//! `crates/vike-telegram/src/failure.rs`'s module doc.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use vike_log::poller::{STOP_POLL_SLICE, StopHandle, sleep_stop_aware, spawn_poller};

// ⚠ SECURITY-CRITICAL, per the table above: `config` (gates + chat allowlist), `confirm` (the
// token contract) and `ledger` (at-most-once). The rest are supporting.
//
// The submodules are PRIVATE and everything public is named at the crate root, so the file layout
// is an internal review affordance, not an API surface.
mod config;
mod confirm;
mod deps;
mod failure;
mod ledger;
mod parse;
mod render;
pub mod telegram_cli;
mod tick;

pub use config::{TELEGRAM_KEYS, TelegramConfig, control_gates_open, parse_chat_ids};
pub use confirm::{Pending, PendingConfirms};
pub use deps::{NodeLink, Outcome, ProdTelegramDependencies, TelegramDependencies};
pub use failure::{BACKOFF_MAX, PollBackoff, PollError, PollStep, status_is_permanent};
pub use ledger::UpdateLedger;
pub use parse::{
    Instruction, ReadVerb, TelegramUpdate, UNKNOWN_USER_ID, confirm_reason, confirmed_by, describe,
    describe_actor, parse_instruction, parse_updates,
};
pub use render::render_read;
pub use tick::{TickReport, poll_once};

/// How long a preview's confirmation token stays valid. Short on purpose: the token authorizes a
/// REAL order, and an operator who meant it confirms within a minute.
pub const CONFIRM_WINDOW_MS: i64 = 60_000;

/// How many previewed-but-unconfirmed commands are held at once (oldest evicted). Small on purpose
/// — a bounded map on a control path, and an operator juggling more than a handful of un-confirmed
/// order previews should re-issue rather than confirm a stale one.
pub const MAX_PENDING: usize = 8;

/// Telegram's `getUpdates` long-poll timeout, seconds. Bounded well under the agent's global
/// timeout, and short enough that a shutdown waits at most this long for the in-flight poll.
pub const LONG_POLL_SECS: u64 = 20;

/// The agent's global request timeout — must exceed [`LONG_POLL_SECS`] or every long-poll would be
/// cut off as a timeout (the reason this channel cannot reuse the alerting agent's 10 s one).
pub const AGENT_TIMEOUT: Duration = Duration::from_secs(LONG_POLL_SECS + 15);

/// Gap between long-polls on a HEALTHY channel. The long-poll itself (20 s) is the wait; this only
/// bounds how fast a raised stop is seen between polls.
///
/// ⚠ It is **not** what keeps a hard-failing endpoint from becoming a hot loop — it used to claim
/// that, and it is exactly false: a failing `getUpdates` returns instantly, so a 500 ms gap IS the
/// hot loop (~2 requests/second). [`PollBackoff`] owns the failure path; this constant is only its
/// starting point.
pub const POLL_GAP: Duration = Duration::from_millis(500);

/// Ledger lines above which [`UpdateLedger::open`] rewrites the file down to its single high-water
/// mark. Append-only is the crash-safe shape; this is what keeps it bounded.
pub const LEDGER_COMPACT_AT: usize = 512;

/// Rows a `/positions` or `/orders` reply prints before truncating.
const MAX_ROWS: usize = 20;

/// The usage line every unrecognized or malformed instruction is answered with.
pub const USAGE: &str = "\
vike-tradehub control. WRITE verbs preview first; nothing is sent until /confirm.
  /status | /positions | /orders | /equity
  <venue> may be <venue>/<LABEL> or <venue>/DEFAULT. A bare <venue> is taken only where the node has ONE
  account of it, and refused elsewhere, naming the books. No venue at all is the whole node.
  /submit <venue> <symbol> <buy|sell> <qty> [price] [reduce]
  /cancel <order_client_id>
  /modify <order_client_id> [qty=<q>] [price=<p>]
  /masscancel [venue] [symbol]
  /flatten <venue> <symbol>
  /marketexit [venue]
  /state <active|reducing|halted>
  /resumevenue <venue> [force]
  /confirm <token>";

// ---------------------------------------------------------------------------------------------
// Start
// ---------------------------------------------------------------------------------------------

/// Decide whether the channel may start, and open what it needs, or return `None` having
/// constructed **nothing**.
///
/// The gate order is the contract (see the crate doc): the two control flags are checked FIRST,
/// purely; only if both are the exact string `"1"` is `load_vars` called at all — which is why it is
/// a FUNCTION, not a value. On the closed path the credential store is never read and the bot token
/// never enters memory.
///
/// The LEDGER is the last precondition, and it is a real gate rather than a path handed onward:
/// opening it proves the file readable and appendable, so a channel whose at-most-once record cannot
/// be kept never arms. `ledger_path` is `Option` because "no project state directory resolves" is
/// one of the ways that can be true, and this function is the single place every reason a channel
/// did not start is reported. Failure is an `error!` and a `None` — never a panic.
pub fn arm<V>(
    tradehub_control: Option<&str>,
    telegram_control: Option<&str>,
    load_vars: V,
    ledger_path: Option<PathBuf>,
) -> Option<(TelegramConfig, UpdateLedger)>
where
    V: FnOnce() -> HashMap<String, String>,
{
    if !control_gates_open(tradehub_control, telegram_control) {
        return None;
    }
    let vars = load_vars();
    let Some(config) = TelegramConfig::from_vars(&vars) else {
        tracing::error!(
            "flags.telegram_control is on but the credential store has no VIKE_TELEGRAM_BOT_TOKEN and/or \
             no VIKE_TELEGRAM_ALLOWED_CHAT_IDS — Telegram control NOT started (absent credential is \
             the gate); the tradehub is untouched"
        );
        return None;
    };
    let Some(path) = ledger_path else {
        tracing::error!(
            "flags.telegram_control is on but no project state directory resolves, so the at-most-once \
             update ledger has nowhere to live — Telegram control NOT started. Name the settings \
             directory ($VIKE_SETTINGS_DIR); the tradehub is untouched"
        );
        return None;
    };
    let ledger = match UpdateLedger::open(&path) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(
                %e,
                "flags.telegram_control is on but the at-most-once update ledger is not usable — \
                 Telegram control NOT started. A remote order path whose replay guard cannot be \
                 written is worse than no remote order path: without it a restart replays \
                 whatever Telegram still holds. Grant the state directory write access (under \
                 systemd: ReadWritePaths=, and NEVER the executable's own directory); the tradehub \
                 is untouched"
            );
            return None;
        }
    };
    tracing::warn!(
        chats = config.chat_count(),
        // 0 = no user allowlist ⇒ EVERY member of every allowlisted chat may command this node.
        // Printed at `warn` beside the chat count precisely so that is visible at enable time,
        // rather than being a property one has to infer from an absent variable.
        users = config.user_count(),
        ledger = ?ledger.path(),
        "flags.telegram_control: Telegram control channel ENABLED — an allowlisted chat may place \
         and cancel REAL orders (every write is preview + /confirm gated, and passes the node's own \
         door, ControlLimits and RiskGate). users=0 means NO per-user allowlist: every member of an \
         allowlisted group chat is authorized"
    );
    Some((config, ledger))
}

/// [`arm`], then the poller thread over deps `make_dependencies` builds — the shape a caller that is not the
/// program uses (the tests). `None` exactly when [`arm`] says so, and then `make_dependencies` never runs.
pub fn maybe_spawn<V, D>(
    tradehub_control: Option<&str>,
    telegram_control: Option<&str>,
    load_vars: V,
    ledger_path: Option<PathBuf>,
    make_dependencies: D,
) -> Option<StopHandle>
where
    V: FnOnce() -> HashMap<String, String>,
    D: FnOnce(&TelegramConfig) -> Box<dyn TelegramDependencies + Send>,
{
    let (config, ledger) = arm(tradehub_control, telegram_control, load_vars, ledger_path)?;
    let dependencies = make_dependencies(&config);
    Some(spawn(config, ledger, dependencies))
}

/// [`run_loop`] on a poller thread of its own, stopped and joined by the handle.
pub fn spawn(
    config: TelegramConfig,
    ledger: UpdateLedger,
    dependencies: Box<dyn TelegramDependencies + Send>,
) -> StopHandle {
    spawn_poller("vike-telegram", move |stop| {
        run_loop(&config, &ledger, dependencies.as_ref(), &stop);
    })
}

/// How [`run_loop`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopEnd {
    /// The stop flag was raised.
    Stopped,
    /// Telegram refused `getUpdates` in a way retrying can never fix (the bot token is wrong or was
    /// revoked): the channel stopped by itself.
    Refused,
}

/// Poll, dispatch and back off until `stop` is raised or Telegram refuses the token for good.
///
/// This is the ONE place a failed pass becomes a delay, a log line, or the end of the channel:
/// [`poll_once`] reports, [`PollBackoff`] decides, and this loop is where both the sleeping and the
/// `tracing` happen. The program runs it on its main thread; [`spawn`] on a thread of its own.
pub fn run_loop(
    config: &TelegramConfig,
    ledger: &UpdateLedger,
    dependencies: &dyn TelegramDependencies,
    stop: &AtomicBool,
) -> LoopEnd {
    let mut pending = PendingConfirms::default();
    let mut backoff = PollBackoff::default();
    while !stop.load(Ordering::Relaxed) {
        let report = poll_once(dependencies, config, ledger, &mut pending);
        let failure = report.poll_failure.as_ref();
        // Never a url and never the token: `ProdTelegramDependencies` puts neither in a `PollError`.
        let reason = failure.map_or("", |e| e.message.as_str());
        let wait = match backoff.observe(failure) {
            PollStep::Ok { wait, recovered_after } => {
                if let Some(after) = recovered_after {
                    tracing::info!(
                        after_failures = after,
                        "telegram control: getUpdates recovered — the channel is live again"
                    );
                }
                wait
            }
            PollStep::Retry { wait, consecutive, announce } => {
                if announce {
                    // Rate-limited to consecutive failures 1, 2, 4, 8, … — immediate at the
                    // start of an outage (an operator whose channel stopped working has to
                    // learn of it) and ~11 lines across a whole day of one.
                    tracing::warn!(
                        %reason,
                        consecutive,
                        retry_in_secs = wait.as_secs_f64(),
                        "telegram control: getUpdates failed — retrying with backoff (this \
                         line is rate-limited: consecutive failures 1, 2, 4, 8, … are \
                         announced, the rest are counted silently)"
                    );
                }
                wait
            }
            PollStep::Stop => {
                tracing::error!(
                    %reason,
                    "telegram control: Telegram REFUSED getUpdates in a way retrying can never \
                     fix — the bot token is wrong, was revoked, or the bot was deleted. The \
                     control channel is now STOPPED rather than retrying ~2x/second forever \
                     against a third-party API. Fix VIKE_TELEGRAM_BOT_TOKEN and RESTART \
                     vike-telegram (the token is read once, at start); the tradehub is untouched"
                );
                return LoopEnd::Refused;
            }
        };
        if sleep_stop_aware(stop, wait, STOP_POLL_SLICE) {
            break;
        }
    }
    LoopEnd::Stopped
}
