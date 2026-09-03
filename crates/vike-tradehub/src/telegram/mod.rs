//! `telegram` — the opt-in Telegram CONTROL channel: a SECOND remote write surface on the headless
//! daemon, reachable from a phone, with **full write** behind a mandatory preview/confirm contract.
//!
//! ## The accepted risk, stated plainly, once
//!
//! With this channel enabled, **possession of the bot token plus membership in an allowlisted chat
//! is sufficient to place real orders.** Telegram is a third party sitting in an order-origination
//! path. `ControlLimits` (notional + rate) and the core `RiskGate` still bound the blast radius, and
//! every gate below applies, but that residual does not go away — it was raised, and the full-write
//! scope was chosen deliberately.
//!
//! ## Layout: the security-critical files are SEPARATE on purpose
//!
//! This module is a directory (the `vike-core/src/runtime/`, `vike-backtest/src/harness/` shape)
//! because the nine jobs it does are not equally dangerous, and interleaving them in one file made
//! that impossible to see. Three files are where a bug means **UNAUTHORIZED ORDER PLACEMENT**:
//!
//! | File | Job | If it is wrong |
//! |---|---|---|
//! | **`config.rs`** ⚠ | the two process-env gates, the bot token, the chat allowlist | a stranger commands the node |
//! | **`confirm.rs`** ⚠ | token single-use / expiry / command+chat binding | a confirmation fires twice, or fires something it never previewed |
//! | **`ledger.rs`** ⚠ | at-most-once across a restart | a restart replays an order |
//! | `parse.rs` | the chat-string grammar → [`Instruction`] | a command is misread — caught by the preview an operator reads before confirming |
//! | `tick.rs` | ONE poll → dispatch pass | the ORDER of dedupe → mark → allowlist → dispatch is itself security-critical |
//! | `render.rs` | read verbs printed off the published snapshot | an ugly message |
//! | `failure.rs` | permanent-vs-transient `getUpdates` failure + the retry schedule | a third party is hammered ~2x/second forever, or a working channel stops |
//! | `deps.rs` | the `ureq` long-poll, the `ControlLimits` bucket, the `CommandSink` | (it holds the token in a URL — hence no `Debug`) |
//!
//! That table is the point of the split: "this change does not touch the security-critical code" is
//! now a property a reviewer reads off the CHANGED-FILE LIST, not one they have to re-derive by
//! reading a 1400-line file.
//!
//! ## Placement: here, NOT in `vike-alerting`
//!
//! `vike-alerting` already delivers OUTBOUND Telegram messages, but its default build has ZERO
//! `vike-*` dependencies — the property its crate split exists to hold — and lowering a chat message
//! into a command names vike types (`WireCommand`, `CommandSink`, `ControlLimits`). The daemon owns
//! the `CommandSink`; the inbound half belongs here.
//!
//! ## ONE acceptance path, shared with the TCP surface
//!
//! This module does NOT re-implement command gating. Every confirmed instruction goes through
//! [`crate::server::accept_command`] — the exact function the TCP `Request::Command` arm calls —
//! so the rate token, the notional cap, the rationale sanitizer, the lowering, the single-writer
//! lane and the audit record are identical **by construction, not by review**. The only
//! surface-specific parts are this channel's own `ControlLimits` bucket (per-surface, exactly as
//! each TCP connection gets its own) and `peer: None` in the audit record — which is why the
//! rationale this channel supplies NAMES its origin (see [`confirm_reason`]).
//!
//! ## Gates — ALL FOUR required simultaneously
//!
//! | Gate | Where | Semantics |
//! |---|---|---|
//! | `VIKE_TRADEHUB_CONTROL=1` | process env (existing) | exact string `"1"` |
//! | `VIKE_TELEGRAM_CONTROL=1` | process env (new) | exact string `"1"` |
//! | `VIKE_TELEGRAM_BOT_TOKEN` | workspace `.env` | absent/blank ⇒ nothing constructed |
//! | `VIKE_TELEGRAM_ALLOWED_CHAT_IDS` | workspace `.env` | comma-separated; EMPTY ⇒ disabled |
//!
//! …plus ONE optional fifth, which can only ever NARROW:
//!
//! | `VIKE_TELEGRAM_ALLOWED_USER_IDS` | workspace `.env` | comma-separated; UNSET ⇒ chat-only |
//!
//! Absent any one of the four ⇒ **no poller thread, no token in memory, no network call**. [`maybe_spawn`]
//! enforces that ordering literally: the two process-env flags are checked by the pure
//! [`control_gates_open`] FIRST, and the workspace `.env` loader is passed as a FUNCTION (the
//! `maybe_mount_alerts` idiom), so on the closed path the `.env` is never even read and the bot
//! token never enters this process's address space.
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
//!    the audit rationale. Before this, an accepted order could be traced no further than
//!    `telegram chat -100123` — an audit trail that cannot name who placed an order, on a surface
//!    that signs real money.
//! 2. **An optional per-user allowlist.** `VIKE_TELEGRAM_ALLOWED_USER_IDS`
//!    ([`TelegramConfig::allows_user`]) is ANDed with the chat check when configured, and unset —
//!    the default — means chat-only, exactly as before. A group deployment can tighten; a DM
//!    deployment is untouched, and nobody is locked out of their own bot by an upgrade.
//!
//! The confirm TOKEN stays chat-bound, not user-bound (see [`Pending::from_id`]): binding it would
//! stop an operator confirming a colleague's preview, which is a change to who may authorize an
//! order and belongs to the opt-in above, not to a default.
//!
//! ## Confirm contract — mirrors the MCP write-tool surface
//!
//! 1. A write instruction is answered with a **preview**: the resolved
//!    [`WireCommand`](vike_tradehub_client::wire::WireCommand), the server-authoritative guardrail
//!    verdict, and a single-use token. **Nothing is sent.**
//! 2. Only `/confirm <token>`, within [`CONFIRM_WINDOW_MS`] (60 s), executes.
//! 3. The token is single-use ([`PendingConfirms::take`] REMOVES it before the command is handed to
//!    the core) and bound to the exact command it previewed AND to the chat that asked for it. A
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
//! is ACKed. An update is marked **BEFORE** it is acted on, deliberately: a daemon that dies
//! mid-command must lose that command, never place it twice.
//!
//! The file is `<project>/settings/state/telegram_updates.ledger` — the one root every
//! program-written file resolves to (`crates/vike-model/src/state_path.rs`'s `project_state_dir`),
//! handed in by the binary as [`LedgerPaths`], which also names the pre-move `<exe_dir>` location so
//! an existing install's mark migrates instead of replaying.
//!
//! ⚠ **A ledger write that cannot land is an ERROR, never a shrug.** It was best-effort once — a
//! `warn!` on the reasoning that the in-memory mark still guards the running session, which is true
//! and beside the point, since the running session is the one case the file is not for. Silently
//! accepting a command you cannot record is the worst available option on an order-origination
//! path, so it is not available:
//!
//! * **arming** — [`maybe_spawn`] opens the ledger, which proves it readable and appendable, and
//!   returns `None` with a `tracing::error!` if it is not. The daemon keeps trading headless; the
//!   remote write surface simply does not exist. This is the same absent-config-is-the-gate shape
//!   the bot token already has, and it fires at startup, where an operator is looking.
//! * **running** — [`UpdateLedger::mark`] returns `Err` and [`poll_once`] drops that update
//!   undispatched (reported as `TickReport::unrecorded`), for the case where the path stopped being
//!   writable under a live daemon.
//!
//! An ALARM-only design was rejected: an alarm that fires while the channel keeps accepting orders
//! leaves the unrecordable-order window wide open, which is precisely the thing that must not remain
//! reachable.
//!
//! ## Transport
//!
//! `getUpdates` long-poll — an outbound HTTPS GET, no inbound listener, no new dependency. Built on
//! [`vike_bridge_core::poller`] (the polymarket `resolve.rs` shape: opt-in exact-`"1"` env, a pure
//! tick function behind a `*Deps` trait seam so the logic tests with NO network, and a `StopHandle`
//! that signals AND joins). It uses its OWN `ureq::Agent` via
//! [`vike_bridge_core::http::blocking_agent_with_timeout`] — the alerting agent's hardcoded 10 s
//! global timeout is shorter than one long-poll.
//!
//! The bot token is embedded in the API base URL, so [`ProdTelegramDeps`] has NO `Debug`, never
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
//! `crates/vike-tradehub/src/telegram/failure.rs`'s module doc.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::Duration;

use vike_bridge_core::poller::{sleep_stop_aware, spawn_poller, StopHandle, STOP_POLL_SLICE};

// ⚠ SECURITY-CRITICAL, per the table above: `config` (gates + chat allowlist), `confirm` (the
// token contract) and `ledger` (at-most-once). The other five are supporting: `parse` (grammar),
// `tick` (the dispatch pass), `render` (read verbs), `failure` (the retry policy), `deps` (the
// production wiring).
//
// The submodules are PRIVATE and everything public is re-exported below, so every existing
// `vike_tradehub::telegram::Item` path (the integration test drives several) is unchanged by the
// split — the file layout is an internal review affordance, not a new API surface.
mod config;
mod confirm;
mod deps;
mod failure;
mod ledger;
mod parse;
mod render;
mod tick;

pub use config::{control_gates_open, parse_chat_ids, TelegramConfig};
pub use confirm::{Pending, PendingConfirms};
pub use deps::{ProdTelegramDeps, TelegramDeps};
pub use failure::{status_is_permanent, PollBackoff, PollError, PollStep, BACKOFF_MAX};
pub use ledger::{LedgerPaths, UpdateLedger};
pub use parse::{
    confirm_reason, confirmed_by, describe, describe_actor, fill_coid, parse_instruction,
    parse_updates, Instruction, ReadVerb, TgUpdate, UNKNOWN_USER_ID,
};
pub use render::render_read;
pub use tick::{poll_once, TickReport};

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
  /submit <venue> <symbol> <buy|sell> <qty> [price] [reduce]
  /cancel <client_order_id>
  /modify <client_order_id> [qty=<q>] [price=<p>]
  /masscancel [venue] [symbol]
  /flatten <venue> <symbol>
  /marketexit [venue]
  /state <active|reducing|halted>
  /confirm <token>";

// ---------------------------------------------------------------------------------------------
// Mount
// ---------------------------------------------------------------------------------------------

/// Owner-side handle: the shared [`StopHandle`] scaffold, so a dropped handle never leaks the
/// poller thread.
pub type TelegramHandle = StopHandle;

/// Mount the channel, or return `None` having constructed **nothing**.
///
/// The gate order is the contract (see the module doc): the two process-env flags are checked
/// FIRST, purely; only if both are the exact string `"1"` is `load_vars` called at all — which is
/// why it is a FUNCTION, not a value. On the closed path the workspace `.env` is never read, the
/// bot token never enters memory, `make_deps` never runs (so no agent is built), and no thread is
/// spawned.
///
/// The LEDGER is the last precondition before anything is constructed, and it is a real gate rather
/// than a path handed onward: opening it proves the file readable and appendable, so a channel
/// whose at-most-once record cannot be kept never arms. `ledger_paths` is `Option` because "no
/// project state directory resolves" is one of the ways that can be true, and this function is the
/// single place every reason a channel did not start is reported. Failure is an `error!` and a
/// `None` — never a panic and never a degraded channel: the daemon keeps trading headless.
pub fn maybe_spawn<V, D>(
    tradehub_control: Option<&str>,
    telegram_control: Option<&str>,
    load_vars: V,
    ledger_paths: Option<LedgerPaths>,
    make_deps: D,
) -> Option<TelegramHandle>
where
    V: FnOnce() -> HashMap<String, String>,
    D: FnOnce(&TelegramConfig) -> Box<dyn TelegramDeps + Send>,
{
    if !control_gates_open(tradehub_control, telegram_control) {
        return None;
    }
    let vars = load_vars();
    let Some(cfg) = TelegramConfig::from_vars(&vars) else {
        tracing::error!(
            "VIKE_TELEGRAM_CONTROL=1 but the workspace .env has no VIKE_TELEGRAM_BOT_TOKEN and/or \
             no VIKE_TELEGRAM_ALLOWED_CHAT_IDS — Telegram control NOT started (absent credential is \
             the gate); the daemon keeps trading headless"
        );
        return None;
    };
    let Some(paths) = ledger_paths else {
        tracing::error!(
            "VIKE_TELEGRAM_CONTROL=1 but no project state directory resolves, so the at-most-once \
             update ledger has nowhere to live — Telegram control NOT started. Name the settings \
             directory ($VIKE_SETTINGS_DIR) or the state root ($VIKE_STATE_ROOT); the daemon keeps \
             trading headless"
        );
        return None;
    };
    let ledger = match UpdateLedger::open(&paths) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(
                %e,
                "VIKE_TELEGRAM_CONTROL=1 but the at-most-once update ledger is not usable — \
                 Telegram control NOT started. A remote order path whose replay guard cannot be \
                 written is worse than no remote order path: without it a daemon restart replays \
                 whatever Telegram still holds. Grant the state directory write access (under \
                 systemd: ReadWritePaths=, and NEVER the executable's own directory); the daemon \
                 keeps trading headless"
            );
            return None;
        }
    };
    tracing::warn!(
        chats = cfg.chat_count(),
        // 0 = no user allowlist ⇒ EVERY member of every allowlisted chat may command this node.
        // Printed at `warn` beside the chat count precisely so that is visible at enable time,
        // rather than being a property one has to infer from an absent variable.
        users = cfg.user_count(),
        ledger = ?ledger.path(),
        "VIKE_TELEGRAM_CONTROL=1: Telegram control channel ENABLED — an allowlisted chat may place \
         and cancel REAL orders (every write is preview + /confirm gated, and passes the same \
         ControlLimits and RiskGate as the TCP control path). users=0 means NO per-user allowlist: \
         every member of an allowlisted group chat is authorized"
    );
    let deps = make_deps(&cfg);
    Some(spawn(cfg, ledger, deps))
}

/// Spawn the poller thread over an already-decided configuration, an already-OPENED ledger, and
/// deps. Every gate stays at the call site ([`maybe_spawn`]) — the [`vike_bridge_core::poller`]
/// contract.
///
/// The ledger arrives open rather than as a path, which is the structural half of the fix: it used
/// to be opened INSIDE the poller thread, where a failure had no caller to report to and nowhere to
/// be reported except a log line nobody reads.
///
/// This is also the ONE place a failed pass becomes a delay, a log line, or the end of the channel:
/// [`poll_once`] reports, [`PollBackoff`] decides, and this loop is where both the sleeping and the
/// `tracing` happen. The thread ENDS on a permanent failure — the handle stays joinable and
/// [`StopHandle::shutdown`] on an already-finished thread is a no-op, so the daemon's teardown is
/// unchanged.
pub fn spawn(
    cfg: TelegramConfig,
    ledger: UpdateLedger,
    deps: Box<dyn TelegramDeps + Send>,
) -> TelegramHandle {
    spawn_poller("vike-tradehub-telegram", move |stop| {
        let mut pending = PendingConfirms::default();
        let mut backoff = PollBackoff::default();
        while !stop.load(Ordering::Relaxed) {
            let report = poll_once(deps.as_ref(), &cfg, &ledger, &mut pending);
            let failure = report.poll_failure.as_ref();
            // Never a url and never the token: `ProdTelegramDeps` puts neither in a `PollError`.
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
                         against a third-party API. Fix VIKE_TELEGRAM_BOT_TOKEN and RESTART (the \
                         token is read once, at mount); the daemon keeps trading headless"
                    );
                    break;
                }
            };
            if sleep_stop_aware(&stop, wait, STOP_POLL_SLICE) {
                break;
            }
        }
    })
}
