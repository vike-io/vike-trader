//! Telegram CONTROL-channel tests.
//!
//! The pure ones drive [`vike_telegram::poll_once`] through the [`TelegramDependencies`] seam with a scripted
//! stub (`telegram_control/deps.rs`), which is the whole reason that seam exists: the gating, the
//! confirm contract and the at-most-once ledger are pure logic over injected updates, an injected
//! clock and an injected nonce source, so they are provable without a bot token, a chat, or a socket.
//!
//! What is proven:
//! - an UNLISTED chat is ignored — no reply of ANY kind, nothing sent (a bot that answers a stranger
//!   is an oracle confirming a trading node is here);
//! - a write instruction NEVER executes on the message that carried it — it only previews;
//! - a confirmation token is single-use, expires after 60 s, and is bound to the exact command
//!   (and chat) it previewed;
//! - a replayed `update_id` is not reprocessed, so a restart cannot re-place an order;
//! - with any gate closed, NOTHING is constructed — the credential store is not even read;
//! - a PERMANENTLY failing `getUpdates` (a wrong bot token) stops the channel after ONE request
//!   instead of retrying ~2x/second forever, while a TRANSIENT one keeps retrying and recovers;
//! - and, against a REAL paper node on loopback (`telegram_control/node_wire.rs`), that the
//!   production deps reach the node's ONE door over the control wire: the node previews and sends,
//!   records the chat's literal text as the rationale, and a chat reads the node's frame.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use vike_telegram::{
    CONFIRM_WINDOW_MS, Outcome, PendingConfirms, PollError, ReadVerb, TelegramConfig,
    TelegramDependencies, TelegramUpdate, UpdateLedger, control_gates_open, maybe_spawn, poll_once,
    spawn,
};
use vike_tradehub_client::wire::CommandWire;

use deps::*;

/// The one allowlisted chat every test uses.
const CHAT: i64 = 4242;
/// A chat that is NOT allowlisted.
const STRANGER: i64 = 99;

#[cfg(test)]
#[path = "telegram_control/acceptance_and_reads.rs"]
mod acceptance_and_reads;
#[cfg(test)]
#[path = "telegram_control/confirm_and_ledger.rs"]
mod confirm_and_ledger;
#[cfg(test)]
#[path = "telegram_control/deps.rs"]
mod deps;
#[cfg(test)]
#[path = "telegram_control/gate_and_auth.rs"]
mod gate_and_auth;
#[cfg(test)]
#[path = "telegram_control/node_wire.rs"]
mod node_wire;
#[cfg(test)]
#[path = "telegram_control/retry_policy.rs"]
mod retry_policy;
