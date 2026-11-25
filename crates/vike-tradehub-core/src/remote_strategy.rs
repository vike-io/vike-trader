//! A Rust strategy FILE mounted live: it runs in its own sandboxed process, and the core holds a proxy
//! (plan 2b §4; decision 0273 items 3-5, with the owner's 2026-11-12 answer Q2: the tradehub spawns the
//! child itself, over a pipe, no strategyhub between them).
//!
//! The mount is an ordinary `vike_core::Mount` whose strategy is a [`RemoteStrategy`]. So its tags,
//! its order client ids, its ownership, its loss budget, its `[risk]` ceilings, the account tier and HALT
//! stay where they are: the core keeps the mount, and only the strategy's CODE is elsewhere.
//!
//! - **The fold never waits on a strategy.** Each hook the core calls on the proxy copies the broker's read
//!   side (a few scalars and `Arc` bumps: [`protocol::ViewRow::of`] over the core's own `LiveBroker`) and
//!   pushes one event onto the mount's bounded queue. The mount's SUPERVISOR thread (`supervisor.rs`) owns the
//!   child: it sends the event, waits for the reply up to [`HOOK_DEADLINE`], and hands the reply's intents to
//!   the core as `vike_execution::Ingest::MountReply`, which the core applies as the mount's own orders through
//!   its one drain (every gate).
//! - **One event in flight per mount.** Quote, book, mark, reference-quote and flow events conflate (the
//!   newest per stream; a declared mount's books per leg) while one is in flight; bars, trades, fills,
//!   order events, schedules, params and feed status queue in order, up to [`QUEUE_CAP`].
//! - **The in-process order holds.** The next event is sent only once the core APPLIED the last reply
//!   (`vike_core::MountReplyIntents::applied`), and what that application delivers to the mount (the drain's
//!   `Denied`, a fill) goes to the head of the queue, so the strategy hears every consequence of its
//!   orders before any hook that reached the core while its reply travelled, as it would in-process. What
//!   cannot hold without the fold waiting: such a hook's broker view is the one the core read when it
//!   folded the hook, before that reply was applied.
//! - **`warmup`, `inputs`, `symbols`** are answered at mount from the child's `Opened`, and `save_state` /
//!   `params` from the last state the child sent (`protocol::STATE_REFRESH`); `load_state` is forwarded.
//!
//! # A failure stops taking that strategy's orders and CANCELS NOTHING (owner, 10-20)
//!
//! The core's own protections keep running (core-held stops, the risk clock, the margin call, HALT), and
//! the strategy's resting orders rest.
//!
//! | failure | what happens |
//! |---|---|
//! | crash, abort, `exit`, OOM, EOF on the pipe | [`RemoteStatus::Restarting`] `(n/5): <exit, stderr tail>`; a fresh child opens with the last cached state (a re-place under a tag a resting order still holds is refused by the core, decision 0277's P1), then hears the order ends queued while it was down |
//! | hang: no reply within [`HOOK_DEADLINE`] | SIGKILL, then as above; the reason names the hook |
//! | garbage: an undecodable frame, a wrong `seq`, an intent naming a symbol the mount does not trade | SIGKILL, NONE of that reply's intents applied, then as above |
//! | a full queue ([`QUEUE_CAP`]) | SIGKILL, the market events over the cap dropped and counted (never an order's end), then as above |
//! | [`CRASH_LIMIT`] crashes in [`CRASH_WINDOW`] | [`RemoteStatus::Stopped`] `crash loop: <last reason>`; events are dropped until an operator restarts the mount |
//! | a state whose bytes are not msgpack (checked once, as a changed state arrives) | [`RemoteStatus::Stopped`] `the strategy's save_state is not valid msgpack: <why>` at once, no restart (a restart would be handed the same bytes); at the first open, the mount does not run |
//! | the first open fails (refused, no answer, no process) | [`RemoteStrategy::open`] answers the reason: that mount does not run, the others do |
//!
//! # Where the child comes from
//!
//! A [`Launcher`] starts it, from the supervisor thread (a child's parent-death signal follows the THREAD
//! that spawned it). In production that is [`SandboxCommand::multicall`]: this same binary's
//! `strategy-sandbox live <builds> <artifact>` verb, started by `vike_sandbox::spawn_strategy_process` (stdin and
//! stdout the protocol, stderr the reason), which jails itself before it loads the artifact and serves the
//! mount with [`serve`]. No hub process ever loads a strategy file. Only Linux runs one: elsewhere the
//! child refuses its jail by name, and the open says so.

pub mod protocol;
mod proxy;
mod serve;
mod supervisor;

pub use proxy::{RemoteHandle, RemoteSpec, RemoteStatus, RemoteStrategy};
pub use serve::serve;
pub use supervisor::{
    CRASH_LIMIT, CRASH_WINDOW, HOOK_DEADLINE, Launched, Launcher, OPEN_DEADLINE, QUEUE_CAP,
    SandboxChild, SandboxCommand,
};

#[cfg(test)]
#[path = "remote_strategy_tests.rs"]
mod remote_strategy_tests;
