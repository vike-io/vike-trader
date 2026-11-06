//! Telegram CONTROL-channel tests — **no test in this file touches the network.**
//!
//! `#![cfg(feature = "telegram")]` — everything this file drives exists only under the crate's
//! off-by-default `telegram` feature. CI compiles and runs it through `ci.yml`'s `tradehub-all` job
//! (`just tradehub-all`); a gated path CI never compiles is exactly how the
//! `benches/engines.rs` `--bench` bug survived (commit 94c74d87 on main).
//!
//! Every one drives [`vike_tradehub::telegram::poll_once`] through the [`TelegramDeps`] seam with a
//! scripted stub, which is the whole reason that seam exists: the gating, the confirm contract and
//! the at-most-once ledger are pure logic over injected updates, an injected clock and an injected
//! nonce source, so they are provable without a bot token, a chat, or a socket.
//!
//! What is proven:
//! - an UNLISTED chat is ignored — no reply of ANY kind, nothing lowered (a bot that answers a
//!   stranger is an oracle confirming a trading node is here);
//! - a write instruction NEVER executes on the message that carried it — it only previews;
//! - a confirmation token is single-use, expires after 60 s, and is bound to the exact command
//!   (and chat) it previewed;
//! - a replayed `update_id` is not reprocessed, so a daemon restart cannot re-place an order;
//! - with any gate closed, NOTHING is constructed — the workspace `.env` is not even read;
//! - a PERMANENTLY failing `getUpdates` (a wrong bot token) stops the channel after ONE request
//!   instead of retrying ~2x/second forever, while a TRANSIENT one keeps retrying and recovers;
//! - a Telegram-origin command hits the SAME `ControlLimits` bucket and produces the same shape of
//!   audit record as a TCP-origin one, because both call
//!   [`vike_tradehub::server::control::accept_command`];
//! - the operator's literal chat text lands in the audit trail as the command's rationale.
//!
//! The last two need a real `vike_core::CommandSink`, so they stand up a PAPER core
//! (`vike_tradehub_core::build_paper_maker_core` — no feed, no creds, no network) and observe the audit trail
//! through the process-global capture `vike_log::capture::global` (see [`audit_capture`], the `control_roundtrip.rs`
//! idiom: `audit::record` fires wherever the caller is, so a thread-local dispatcher is not enough).
#![cfg(feature = "telegram")]

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_tradehub::server::control::{ControlLimits, ControlLimitsConfig, accept_command};
use vike_tradehub::telegram::{
    CONFIRM_WINDOW_MS, PendingConfirms, PollError, ProdTelegramDeps, ReadVerb, TelegramConfig,
    TelegramDeps, TgUpdate, UpdateLedger, control_gates_open, maybe_spawn, poll_once, spawn,
};
use vike_tradehub_client::wire::{WireCommand, WireOrderRequest};
use vike_tradehub_core::{MakerConfig, build_paper_maker_core};

use audit_capture::{audit_entry_for, test_init};
use deps::*;

/// The one allowlisted chat every test uses.
const CHAT: i64 = 4242;
/// A chat that is NOT allowlisted.
const STRANGER: i64 = 99;

const TOKEN: &str = "TELEGRAM_CONTROL_TOKEN";
/// Far-future resolution so the A-S horizon is positive (the `control_roundtrip.rs` mount shape).
const RESOLUTION_TS: i64 = 3_000_000_000;

// ---------------------------------------------------------------------------------------------
// Audit capture (the `control_roundtrip.rs` idiom)
// ---------------------------------------------------------------------------------------------

/// Observing the AUDIT trail from an integration test.
///
/// `audit::record` emits ONE `tracing::info!` event on whichever thread accepted the command, which a
/// THREAD-LOCAL capture cannot see, so the capture is the process-global one, `vike_log::capture::global`,
/// enabled ONLY for the `vike_tradehub::audit` target. Every test that needs it calls [`test_init`] (in
/// place of `vike_log::test_init`) so the install cannot lose the one-global-subscriber race.
mod audit_capture {
    use vike_log::capture::global;

    /// The tracing target `vike_tradehub::audit`'s events carry (the module path).
    const AUDIT_TARGET: &str = "vike_tradehub::audit";

    /// One captured audit record: the command verb, the client-order-id, and the RECORDED
    /// rationale (`None` when the event carried no `reason` field at all).
    #[derive(Debug, Clone, PartialEq)]
    pub struct AuditEntry {
        pub kind: String,
        pub coid: String,
        pub reason: Option<String>,
    }

    /// Install the capture exactly once for this test binary (best-effort by design: a failed
    /// install makes the capture EMPTY, which the audit tests then fail on loudly).
    pub fn test_init() {
        global(AUDIT_TARGET);
    }

    /// The audit entry recorded for `coid`, waited on briefly (`audit::record` runs on whichever
    /// thread accepted the command, so this is belt-and-braces rather than a race the assertion
    /// depends on).
    pub fn audit_entry_for(coid: &str) -> Option<AuditEntry> {
        for _ in 0..200 {
            if let Some(e) = global(AUDIT_TARGET).iter().find(|e| e.field("coid") == Some(coid)) {
                return Some(AuditEntry {
                    kind: e.field("kind").unwrap_or_default().to_string(),
                    coid: coid.to_string(),
                    reason: e.field("reason").map(str::to_string),
                });
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        None
    }
}

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
#[path = "telegram_control/foreign_pair_preview.rs"]
mod foreign_pair_preview;
#[cfg(test)]
#[path = "telegram_control/gate_and_auth.rs"]
mod gate_and_auth;
#[cfg(test)]
#[path = "telegram_control/prod_deps_rosters.rs"]
mod prod_deps_rosters;
#[cfg(test)]
#[path = "telegram_control/retry_policy.rs"]
mod retry_policy;
