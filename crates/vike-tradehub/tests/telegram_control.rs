//! Telegram CONTROL-channel tests — **no test in this file touches the network.**
//!
//! `#![cfg(feature = "telegram")]` — everything this file drives exists only under the crate's
//! off-by-default `telegram` feature. CI compiles and runs it through the `telegram` feature suite
//! (`bash scripts/ci_feature_suite.sh telegram`); a gated path CI never compiles is exactly how the
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
//!   [`vike_tradehub::server::accept_command`];
//! - the operator's literal chat text lands in the audit trail as the command's rationale.
//!
//! The last two need a real `vike_core::CommandSink`, so they stand up a PAPER core
//! (`vike_run::build_paper_maker_core` — no feed, no creds, no network) and observe the audit trail
//! through a hand-rolled capture subscriber (see [`audit_capture`], the `control_roundtrip.rs`
//! idiom: `audit::record` fires wherever the caller is, so a thread-local dispatcher is not enough).
#![cfg(feature = "telegram")]

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_run::{build_paper_maker_core, MakerMountConfig};
use vike_tradehub::server::{accept_command, ControlLimits, ControlLimitsConfig};
use vike_tradehub::telegram::{
    control_gates_open, maybe_spawn, poll_once, spawn, LedgerPaths, PendingConfirms, PollError,
    ReadVerb, TelegramConfig, TelegramDeps, TgUpdate, UpdateLedger, CONFIRM_WINDOW_MS,
};
use vike_tradehub_client::wire::{WireCommand, WireOrderRequest};

use audit_capture::{audit_entry_for, test_init};

/// The one allowlisted chat every test uses.
const CHAT: i64 = 4242;
/// A chat that is NOT allowlisted.
const STRANGER: i64 = 99;

const TOKEN: &str = "TELEGRAM_CONTROL_TOKEN";
/// Far-future resolution so the A-S horizon is positive (the `control_roundtrip.rs` mount shape).
const RESOLUTION_TS: i64 = 3_000_000_000;

// ---------------------------------------------------------------------------------------------
// The scripted deps
// ---------------------------------------------------------------------------------------------

type PreviewFn = Box<dyn Fn(&WireCommand) -> Option<String> + Send>;
type AcceptFn = Box<dyn Fn(WireCommand, &str) -> Result<String, String> + Send>;

/// Everything the stub observed, so a test can assert on what the channel DID rather than only on
/// what it reported.
#[derive(Default)]
struct Recorded {
    /// `offset` passed to each `getUpdates` — the Telegram ACK, and the restart-replay guard.
    offsets: Vec<i64>,
    /// every reply actually sent, with its target chat.
    replies: Vec<(i64, String)>,
    /// every command that reached `accept` — i.e. everything that could move an order.
    accepted: Vec<(WireCommand, String)>,
}

/// The scripted [`TelegramDeps`]: canned update batches, an injected clock, deterministic nonces,
/// and pluggable preview/accept hooks (used by the tests that wire the REAL shared path).
struct StubDeps {
    batches: Mutex<VecDeque<Vec<TgUpdate>>>,
    rec: Mutex<Recorded>,
    now: AtomicI64,
    coid_seq: AtomicUsize,
    token_seq: AtomicUsize,
    preview_fn: Option<PreviewFn>,
    accept_fn: Option<AcceptFn>,
}

impl StubDeps {
    fn new() -> Self {
        StubDeps {
            batches: Mutex::new(VecDeque::new()),
            rec: Mutex::new(Recorded::default()),
            now: AtomicI64::new(0),
            coid_seq: AtomicUsize::new(0),
            token_seq: AtomicUsize::new(0),
            preview_fn: None,
            accept_fn: None,
        }
    }

    /// Queue one `getUpdates` batch (one per [`poll_once`] call, in order).
    fn push_batch(&self, updates: Vec<TgUpdate>) {
        self.batches.lock().unwrap().push_back(updates);
    }

    fn set_now(&self, ms: i64) {
        self.now.store(ms, Ordering::Relaxed);
    }

    fn with_preview(mut self, f: PreviewFn) -> Self {
        self.preview_fn = Some(f);
        self
    }

    fn with_accept(mut self, f: AcceptFn) -> Self {
        self.accept_fn = Some(f);
        self
    }

    fn accepted(&self) -> Vec<(WireCommand, String)> {
        self.rec.lock().unwrap().accepted.clone()
    }

    fn replies(&self) -> Vec<(i64, String)> {
        self.rec.lock().unwrap().replies.clone()
    }

    fn offsets(&self) -> Vec<i64> {
        self.rec.lock().unwrap().offsets.clone()
    }
}

impl TelegramDeps for StubDeps {
    fn get_updates(&self, offset: i64) -> Result<Vec<TgUpdate>, PollError> {
        self.rec.lock().unwrap().offsets.push(offset);
        Ok(self.batches.lock().unwrap().pop_front().unwrap_or_default())
    }

    fn send_message(&self, chat_id: i64, text: &str) {
        self.rec.lock().unwrap().replies.push((chat_id, text.to_string()));
    }

    fn preview(&self, cmd: &WireCommand) -> Option<String> {
        match &self.preview_fn {
            Some(f) => f(cmd),
            None => None, // default: the guardrail passes
        }
    }

    fn accept(&self, cmd: WireCommand, reason: &str) -> Result<String, String> {
        self.rec.lock().unwrap().accepted.push((cmd.clone(), reason.to_string()));
        match &self.accept_fn {
            Some(f) => f(cmd, reason),
            None => Ok(coid_of(&cmd)),
        }
    }

    fn read(&self, verb: ReadVerb) -> String {
        format!("read:{verb:?}")
    }

    fn mint_coid(&self) -> String {
        format!("tg-coid-{}", self.coid_seq.fetch_add(1, Ordering::Relaxed))
    }

    fn mint_token(&self) -> String {
        format!("tok{}", self.token_seq.fetch_add(1, Ordering::Relaxed))
    }

    fn now_ms(&self) -> i64 {
        self.now.load(Ordering::Relaxed)
    }
}

/// The coid a command targets, for the stub's default `accept` answer (mirrors what
/// `server::lower_command` would echo).
fn coid_of(cmd: &WireCommand) -> String {
    match cmd {
        WireCommand::Submit(r) => r.client_order_id.clone(),
        WireCommand::Cancel(c) => c.clone(),
        WireCommand::Modify { client_order_id, .. } => client_order_id.clone(),
        _ => String::new(),
    }
}

/// The default sender for the harness — one operator, the DM shape.
const USER: i64 = 55;
/// A SECOND sender in the same allowlisted chat: the group shape, and the case a chat-only
/// allowlist cannot distinguish at all.
const OTHER_USER: i64 = 66;

fn update(id: i64, chat: i64, text: &str) -> TgUpdate {
    update_from(id, chat, USER, text)
}

/// An update from a NAMED sender — the group case.
fn update_from(id: i64, chat: i64, from_id: i64, text: &str) -> TgUpdate {
    TgUpdate {
        update_id: id,
        chat_id: chat,
        from_id,
        from_username: Some(format!("u{from_id}")),
        text: text.to_string(),
    }
}

/// A config allowlisting exactly [`CHAT`], with NO user allowlist — the default, chat-only shape.
fn config() -> TelegramConfig {
    TelegramConfig::new("123456:test-token", vec![CHAT]).expect("a token + one chat configures it")
}

/// The same config TIGHTENED to a single user — the opt-in a group deployment can turn on.
fn config_user_allowlisted() -> TelegramConfig {
    config().allowing_users(vec![USER])
}

/// A fresh ledger in a throwaway directory. The `TempDir` is returned so the caller keeps it alive.
fn ledger() -> (tempfile::TempDir, UpdateLedger) {
    let dir = tempfile::tempdir().expect("tempdir");
    let ledger = UpdateLedger::open(&ledger_paths_in(dir.path())).expect("a writable temp dir");
    (dir, ledger)
}

/// [`LedgerPaths`] under `dir`, with no legacy half — the steady state after the move.
fn ledger_paths_in(dir: &Path) -> LedgerPaths {
    LedgerPaths { path: dir.join("telegram_updates.ledger"), legacy: None }
}

/// A `LedgerPaths` pointing INSIDE a plain file, so neither the directory nor the ledger can be
/// created. The portable stand-in for the read-only `<exe_dir>` this channel used to write into
/// (`crates/vike-model/src/state_path.rs`'s `an_uncreatable_state_dir_errors_instead_of_panicking`
/// uses the same trick; a mode-000 directory does not work on the Windows dev box).
fn unwritable_ledger_paths(dir: &Path) -> LedgerPaths {
    let blocker = dir.join("blocker");
    std::fs::write(&blocker, "not a directory").expect("blocker");
    LedgerPaths { path: blocker.join("state").join("telegram_updates.ledger"), legacy: None }
}

// ---------------------------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------------------------

/// With ANY of the four gates closed, NOTHING is constructed — and crucially the workspace `.env`
/// is not even READ, so the bot token never enters the process. That is the property the
/// function-not-value `load_vars` parameter exists to make provable.
#[test]
fn all_gates_absent_constructs_nothing() {
    // Counters the two lazily-invoked constructors bump, so "nothing was constructed" is an
    // observation rather than an assumption.
    let loads = Arc::new(AtomicUsize::new(0));
    let builds = Arc::new(AtomicUsize::new(0));
    // A perfectly USABLE ledger throughout, so every `None` below is attributable to the gate under
    // test and never to the ledger precondition. It is never even opened — the file's absence at
    // the end of this test is itself the proof that a shut gate short-circuits before it.
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = ledger_paths_in(dir.path());

    let closed: &[(Option<&str>, Option<&str>)] = &[
        (None, None),              // neither flag
        (Some("1"), None),         // tradehub control only
        (None, Some("1")),         // telegram control only
        (Some("1"), Some("true")), // a fuzzy truthy spelling arms NOTHING …
        (Some("true"), Some("1")), // … in either position
        (Some("0"), Some("1")),    // an explicit off
        (Some("1"), Some("")),     // set-but-blank is not "1"
        (Some("1"), Some(" 1 ")),  // untrimmed is not the EXACT string
    ];
    for (tradehub, telegram) in closed {
        assert!(
            !control_gates_open(*tradehub, *telegram),
            "{tradehub:?}/{telegram:?} must be shut"
        );
        let (l, b) = (Arc::clone(&loads), Arc::clone(&builds));
        let handle = maybe_spawn(
            *tradehub,
            *telegram,
            || {
                l.fetch_add(1, Ordering::Relaxed);
                HashMap::new()
            },
            Some(paths.clone()),
            |_cfg| {
                b.fetch_add(1, Ordering::Relaxed);
                Box::new(StubDeps::new()) as Box<dyn TelegramDeps + Send>
            },
        );
        assert!(handle.is_none(), "{tradehub:?}/{telegram:?} must not spawn a poller");
    }
    assert_eq!(
        loads.load(Ordering::Relaxed),
        0,
        "the workspace .env must NOT be read while a process-env gate is shut — that is what keeps \
         the bot token out of memory"
    );
    assert_eq!(builds.load(Ordering::Relaxed), 0, "no agent/deps may be constructed either");

    // Both flags open, but NO credentials in the `.env`: the loader runs (that is how we learn),
    // the deps are still never built and no thread is spawned.
    let (l, b) = (Arc::clone(&loads), Arc::clone(&builds));
    let handle = maybe_spawn(
        Some("1"),
        Some("1"),
        || {
            l.fetch_add(1, Ordering::Relaxed);
            HashMap::new()
        },
        Some(paths.clone()),
        |_cfg| {
            b.fetch_add(1, Ordering::Relaxed);
            Box::new(StubDeps::new()) as Box<dyn TelegramDeps + Send>
        },
    );
    assert!(handle.is_none(), "absent credentials are the gate — no channel");
    assert_eq!(loads.load(Ordering::Relaxed), 1, "the loader runs exactly once on the open path");
    assert_eq!(builds.load(Ordering::Relaxed), 0, "still nothing constructed");

    // A token WITHOUT an allowlist is likewise nothing — an empty allowlist can never mean
    // "any chat".
    let vars: HashMap<String, String> = [
        ("VIKE_TELEGRAM_BOT_TOKEN".to_string(), "123:abc".to_string()),
        ("VIKE_TELEGRAM_ALLOWED_CHAT_IDS".to_string(), String::new()),
    ]
    .into_iter()
    .collect();
    let b = Arc::clone(&builds);
    let handle = maybe_spawn(
        Some("1"),
        Some("1"),
        move || vars,
        Some(paths.clone()),
        |_cfg| {
            b.fetch_add(1, Ordering::Relaxed);
            Box::new(StubDeps::new()) as Box<dyn TelegramDeps + Send>
        },
    );
    assert!(handle.is_none(), "an empty allowlist disables the channel");
    assert_eq!(builds.load(Ordering::Relaxed), 0);
}

// ---------------------------------------------------------------------------------------------
// Authorization
// ---------------------------------------------------------------------------------------------

/// A message from a chat that is not on the allowlist is dropped: nothing is previewed, nothing is
/// lowered, and — the part that matters — **no reply of any kind is sent**. A reply would confirm
/// to a stranger holding a leaked bot token that a trading node is on the other end.
#[test]
fn unlisted_chat_is_ignored() {
    let deps = StubDeps::new();
    deps.push_batch(vec![
        update(1, STRANGER, "/submit hyperliquid BTC buy 100 64000"),
        update(2, STRANGER, "/status"),
        update(3, STRANGER, "/confirm anything"),
    ]);
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();
    let report = poll_once(&deps, &config(), &led, &mut pending);

    assert_eq!(report.ignored_unlisted, vec![STRANGER, STRANGER, STRANGER]);
    assert_eq!(report.replies, 0, "an unlisted chat is NEVER answered");
    assert!(deps.replies().is_empty(), "not one byte goes back to the stranger");
    assert!(report.previewed.is_empty() && report.executed.is_empty());
    assert!(deps.accepted().is_empty(), "nothing was ever lowered toward the core");
    assert!(pending.is_empty(), "no token was minted for a stranger");
    // The updates are still CONSUMED, so Telegram stops redelivering them (the offset advances).
    assert!(led.is_processed(3));
    assert_eq!(led.offset(), 4);
}

/// ⚠ **THE DEFAULT IS PER-CHAT, AND A CHAT IS NOT A PERSON.** With no user allowlist configured,
/// any sender in an allowlisted chat commands the node — the documented policy, pinned here so it
/// cannot change silently in either direction. What it must NOT be is unattributable: the audit
/// rationale names the sender, so an accepted order in a group can be traced to a person.
#[test]
fn chat_only_authorization_admits_any_member_but_records_who() {
    let deps = StubDeps::new();
    deps.push_batch(vec![update_from(1, CHAT, OTHER_USER, "/marketexit hyperliquid")]);
    deps.push_batch(vec![update_from(2, CHAT, OTHER_USER, "/confirm tok0")]);
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();

    let previewed = poll_once(&deps, &config(), &led, &mut pending);
    assert_eq!(previewed.previewed, vec!["tok0"], "a second group member may command the node");
    assert!(previewed.ignored_unlisted_user.is_empty(), "no user allowlist ⇒ nothing user-refused");

    let executed = poll_once(&deps, &config(), &led, &mut pending);
    assert_eq!(executed.executed.len(), 1, "and their /confirm executes");
    let (_, reason) = deps.accepted().pop().expect("one command reached accept");
    assert!(
        reason.contains(&format!("user {OTHER_USER}")),
        "the audit rationale must name the PERSON, not just the chat: {reason}"
    );
    assert!(reason.contains(&format!("chat {CHAT}")), "…and still the chat: {reason}");
}

/// The OPT-IN tightening. With `VIKE_TELEGRAM_ALLOWED_USER_IDS` configured, a chat-allowlisted but
/// user-unlisted sender is refused — and, exactly like an unlisted chat, is **never answered**.
#[test]
fn a_configured_user_allowlist_refuses_an_unlisted_member_silently() {
    let deps = StubDeps::new();
    deps.push_batch(vec![
        update_from(1, CHAT, OTHER_USER, "/submit hyperliquid BTC buy 100 64000"),
        update_from(2, CHAT, OTHER_USER, "/status"),
        update_from(3, CHAT, OTHER_USER, "/confirm anything"),
    ]);
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();
    let report = poll_once(&deps, &config_user_allowlisted(), &led, &mut pending);

    assert_eq!(report.ignored_unlisted_user, vec![OTHER_USER; 3]);
    assert!(
        report.ignored_unlisted.is_empty(),
        "the CHAT passed — it is the USER that was refused"
    );
    assert_eq!(
        report.replies, 0,
        "same discipline as an unlisted chat: no reply, not even an error"
    );
    assert!(deps.replies().is_empty());
    assert!(deps.accepted().is_empty(), "nothing was ever lowered toward the core");
    assert!(pending.is_empty(), "no token minted for an unlisted member");
    // …and the updates are still consumed, so Telegram stops redelivering them.
    assert_eq!(led.offset(), 4);
}

/// …while the allowlisted operator in that same chat is unaffected — the tightening narrows, it
/// does not break the deployment.
#[test]
fn a_configured_user_allowlist_still_admits_its_own_member() {
    let deps = StubDeps::new();
    deps.push_batch(vec![update_from(1, CHAT, USER, "/marketexit hyperliquid")]);
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();
    let report = poll_once(&deps, &config_user_allowlisted(), &led, &mut pending);
    assert_eq!(report.previewed, vec!["tok0"]);
    assert!(report.ignored_unlisted_user.is_empty());
}

/// PREVIEW and CONFIRM can be two different people, because the token binds to the CHAT. That is
/// deliberately unchanged — but the audit line must then name BOTH, or an order looks like it was
/// authorized by whoever happened to type the instruction.
#[test]
fn a_cross_user_confirm_records_both_actors() {
    let deps = StubDeps::new();
    deps.push_batch(vec![update_from(1, CHAT, USER, "/marketexit hyperliquid")]);
    deps.push_batch(vec![update_from(2, CHAT, OTHER_USER, "/confirm tok0")]);
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();

    poll_once(&deps, &config(), &led, &mut pending);
    let executed = poll_once(&deps, &config(), &led, &mut pending);
    assert_eq!(executed.executed.len(), 1, "another member of the chat CAN confirm — unchanged");

    let (_, reason) = deps.accepted().pop().expect("one command reached accept");
    assert!(reason.contains(&format!("user {USER}")), "who instructed: {reason}");
    assert!(reason.contains(&format!("/confirm by user {OTHER_USER}")), "who authorized: {reason}");
}

// ---------------------------------------------------------------------------------------------
// The confirm contract
// ---------------------------------------------------------------------------------------------

/// THE mandatory-preview contract: the message carrying a write instruction can never execute it.
/// It answers with a preview + a token and stops there.
#[test]
fn write_without_confirm_executes_nothing() {
    let deps = StubDeps::new();
    deps.push_batch(vec![update(1, CHAT, "/submit hyperliquid BTC buy 0.5 64000")]);
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();
    let report = poll_once(&deps, &config(), &led, &mut pending);

    assert_eq!(report.previewed.len(), 1, "exactly one token issued");
    assert!(report.executed.is_empty(), "NOTHING was sent");
    assert!(deps.accepted().is_empty(), "`accept` — the only path to the core — was never called");
    assert_eq!(pending.len(), 1, "the command is held, not sent");

    let (chat, text) = deps.replies().first().cloned().expect("the operator gets a preview back");
    assert_eq!(chat, CHAT);
    assert!(text.contains("PREVIEW"), "{text}");
    assert!(text.contains("nothing has been sent"), "{text}");
    assert!(text.contains("/confirm"), "the reply must say how to execute: {text}");
    // The preview shows the RESOLVED command, including the coid minted for it.
    for needle in ["hyperliquid", "BTC", "buy", "0.5", "64000", "tg-coid-0"] {
        assert!(text.contains(needle), "{needle:?} missing from the preview: {text}");
    }
}

/// A token fires at most once. The second `/confirm` of the same token finds nothing — the entry is
/// REMOVED before the command is handed on, so even a duplicated message cannot double-place.
#[test]
fn confirm_token_is_single_use() {
    let deps = StubDeps::new();
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();

    deps.push_batch(vec![update(1, CHAT, "/cancel ORDER-A")]);
    let previewed = poll_once(&deps, &config(), &led, &mut pending);
    let token = previewed.previewed.first().cloned().expect("a token was issued");

    // Both confirms arrive in ONE batch, so this is not a timing artifact.
    deps.push_batch(vec![
        update(2, CHAT, &format!("/confirm {token}")),
        update(3, CHAT, &format!("/confirm {token}")),
    ]);
    let report = poll_once(&deps, &config(), &led, &mut pending);

    assert_eq!(report.executed.len(), 1, "exactly one execution");
    assert_eq!(deps.accepted().len(), 1, "the core saw the command exactly once");
    assert!(pending.is_empty(), "the token is gone after firing");
    let last = deps.replies().last().cloned().expect("a reply to the second confirm").1;
    assert!(last.contains("no such pending confirmation"), "{last}");
}

/// Past the 60 s window the token is refused and the command is dropped — an operator who walked
/// away cannot have a stale intent execute later.
#[test]
fn confirm_token_expires() {
    let deps = StubDeps::new();
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();

    deps.set_now(1_000_000);
    deps.push_batch(vec![update(1, CHAT, "/cancel ORDER-A")]);
    let previewed = poll_once(&deps, &config(), &led, &mut pending);
    let token = previewed.previewed.first().cloned().expect("a token was issued");

    // One millisecond past the window.
    deps.set_now(1_000_000 + CONFIRM_WINDOW_MS + 1);
    deps.push_batch(vec![update(2, CHAT, &format!("/confirm {token}"))]);
    let report = poll_once(&deps, &config(), &led, &mut pending);

    assert_eq!(report.expired, vec![token], "reported as EXPIRED, not as unknown");
    assert!(report.executed.is_empty());
    assert!(deps.accepted().is_empty(), "an expired confirmation never reaches the core");
    assert!(pending.is_empty(), "the expired token is burned, not left for a later retry");
    let last = deps.replies().last().cloned().expect("a reply").1;
    assert!(last.contains("EXPIRED"), "{last}");

    // Exactly AT the window is still valid (the boundary is not off by one).
    deps.set_now(2_000_000);
    deps.push_batch(vec![update(3, CHAT, "/cancel ORDER-B")]);
    let previewed = poll_once(&deps, &config(), &led, &mut pending);
    let token = previewed.previewed.first().cloned().expect("a token was issued");
    deps.set_now(2_000_000 + CONFIRM_WINDOW_MS);
    deps.push_batch(vec![update(4, CHAT, &format!("/confirm {token}"))]);
    let report = poll_once(&deps, &config(), &led, &mut pending);
    assert_eq!(report.executed, vec!["ORDER-B".to_string()]);
}

/// A token executes the command it PREVIEWED — never a later one. Two previews are outstanding; the
/// first token executes the first command, verbatim, and the second stays untouched.
#[test]
fn confirm_token_bound_to_command() {
    let deps = StubDeps::new();
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();

    deps.push_batch(vec![update(1, CHAT, "/cancel ORDER-A"), update(2, CHAT, "/cancel ORDER-B")]);
    let previewed = poll_once(&deps, &config(), &led, &mut pending);
    assert_eq!(previewed.previewed.len(), 2, "two independent previews");
    let (token_a, token_b) = (previewed.previewed[0].clone(), previewed.previewed[1].clone());
    assert_ne!(token_a, token_b);

    deps.push_batch(vec![update(3, CHAT, &format!("/confirm {token_a}"))]);
    let report = poll_once(&deps, &config(), &led, &mut pending);

    assert_eq!(report.executed, vec!["ORDER-A".to_string()]);
    let accepted = deps.accepted();
    assert_eq!(accepted.len(), 1);
    assert_eq!(
        accepted[0].0,
        WireCommand::Cancel("ORDER-A".into()),
        "token A executed exactly the command A previewed — never B's"
    );
    assert_eq!(pending.len(), 1, "B's token is untouched");

    // And a token cannot be spent from a DIFFERENT chat, even the right token.
    deps.push_batch(vec![update(4, STRANGER, &format!("/confirm {token_b}"))]);
    let report = poll_once(&deps, &config(), &led, &mut pending);
    assert_eq!(report.ignored_unlisted, vec![STRANGER]);
    assert_eq!(deps.accepted().len(), 1, "still exactly one command ever reached the core");
    assert_eq!(pending.len(), 1, "B's token survived the stranger's attempt");
}

// ---------------------------------------------------------------------------------------------
// At-most-once
// ---------------------------------------------------------------------------------------------

/// Telegram redelivers anything below the acked offset, and a restarted daemon re-reads the ledger.
/// Neither may re-execute a command: a replayed `update_id` is skipped outright, and the next poll
/// asks for `last + 1`.
#[test]
fn duplicate_update_id_is_not_reprocessed() {
    let deps = StubDeps::new();
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();

    deps.push_batch(vec![update(5, CHAT, "/cancel ORDER-A")]);
    let previewed = poll_once(&deps, &config(), &led, &mut pending);
    let token = previewed.previewed.first().cloned().expect("a token was issued");

    deps.push_batch(vec![update(6, CHAT, &format!("/confirm {token}"))]);
    let first = poll_once(&deps, &config(), &led, &mut pending);
    assert_eq!(first.executed, vec!["ORDER-A".to_string()]);
    assert_eq!(deps.accepted().len(), 1);

    // Telegram redelivers BOTH updates (an ack that never landed / a restart).
    deps.push_batch(vec![
        update(5, CHAT, "/cancel ORDER-A"),
        update(6, CHAT, &format!("/confirm {token}")),
    ]);
    let replayed = poll_once(&deps, &config(), &led, &mut pending);

    assert_eq!(replayed.skipped_duplicate, vec![5, 6]);
    assert!(replayed.previewed.is_empty() && replayed.executed.is_empty());
    assert_eq!(deps.accepted().len(), 1, "the order was NOT placed a second time");
    assert_eq!(replayed.replies, 0, "a replayed update is not even answered");

    // The ACK: each poll asks for last + 1, so Telegram drops what we already consumed.
    assert_eq!(deps.offsets(), vec![1, 6, 7]);
    assert_eq!(led.last(), 6);
}

/// ⚠ **THE DEFECT, gated.** This file's whole job is at-most-once ACROSS A RESTART, and its
/// persistence used to be best-effort: a write that could not land was swallowed, so the in-memory
/// mark held for the running process and the NEXT one started from `0` — i.e. it replayed
/// everything Telegram still held.
///
/// Not hypothetical. The ledger used to live beside the EXECUTABLE (`<project>/bin/` for
/// `deploy/vike-tradehub.service`'s `ExecStart`), and that unit runs `ProtectSystem=strict`, which
/// makes the whole filesystem read-only except what `ReadWritePaths=` names. Every append failed,
/// every failure was swallowed, and the at-most-once record of a REMOTE ORDER-ORIGINATION path was
/// silently absent.
///
/// This test is the RED-before gate: against the pre-fix code it asserted the mark survived a
/// restart and FAILED (`left: 0, right: 7`). The contract it holds now is the fix's — the write
/// cannot silently no-op because it cannot silently do anything.
#[test]
fn a_ledger_write_that_cannot_land_is_an_error_not_a_silent_no_op() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = unwritable_ledger_paths(dir.path());

    let err = UpdateLedger::open(&paths).expect_err("an unwritable ledger must refuse to open");
    assert!(
        err.contains(&paths.path.display().to_string()),
        "the error names the path an operator has to fix: {err}"
    );

    // And the runtime half: a directory that stops being writable UNDER a live daemon. The mark
    // still advances in memory (it is the Telegram ACK — see `UpdateLedger::mark`), but the caller
    // is TOLD, which is what lets `poll_once` refuse to dispatch.
    let live = tempfile::tempdir().expect("tempdir");
    let ledger = UpdateLedger::open(&ledger_paths_in(live.path())).expect("writable at open");
    ledger.mark(7).expect("the first append lands");
    let path = live.path().join("telegram_updates.ledger");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir_all(&path).unwrap(); // a DIRECTORY where the file was
    assert!(ledger.mark(8).is_err(), "an append that cannot land must report it, never shrug");
}

/// The ARMING gate: a channel whose at-most-once record cannot be kept never starts. Nothing is
/// constructed — no `ureq` agent, no bot token in a struct, no poller thread — because a remote
/// order path without its replay guard is worse than no remote order path.
///
/// This is the production failure shape made loud: `ProtectSystem=strict` + a ledger path the unit
/// grants no write access to. RED before the fix (`maybe_spawn` never opened the ledger at all, so
/// it returned `Some` and built the deps).
#[test]
fn an_unwritable_ledger_refuses_to_arm_the_channel() {
    let dir = tempfile::tempdir().expect("tempdir");
    let builds = Arc::new(AtomicUsize::new(0));
    let vars: HashMap<String, String> = [
        ("VIKE_TELEGRAM_BOT_TOKEN".to_string(), "123:abc".to_string()),
        ("VIKE_TELEGRAM_ALLOWED_CHAT_IDS".to_string(), CHAT.to_string()),
    ]
    .into_iter()
    .collect();

    for paths in [Some(unwritable_ledger_paths(dir.path())), None] {
        let b = Arc::clone(&builds);
        let v = vars.clone();
        let handle = maybe_spawn(
            Some("1"),
            Some("1"),
            move || v,
            paths,
            |_cfg| {
                b.fetch_add(1, Ordering::Relaxed);
                Box::new(StubDeps::new()) as Box<dyn TelegramDeps + Send>
            },
        );
        assert!(handle.is_none(), "no usable ledger ⇒ no channel");
    }
    assert_eq!(
        builds.load(Ordering::Relaxed),
        0,
        "the deps (and with them the bot-token-bearing agent) are never built"
    );

    // …and the control: the SAME configuration with a writable ledger DOES arm, so the assertions
    // above are about the ledger and not about some other gate being shut.
    let ok = tempfile::tempdir().expect("tempdir");
    let b = Arc::clone(&builds);
    let handle = maybe_spawn(
        Some("1"),
        Some("1"),
        move || vars,
        Some(ledger_paths_in(ok.path())),
        |_cfg| {
            b.fetch_add(1, Ordering::Relaxed);
            Box::new(StubDeps::new()) as Box<dyn TelegramDeps + Send>
        },
    );
    assert!(handle.is_some(), "a writable ledger arms the channel");
    assert_eq!(builds.load(Ordering::Relaxed), 1);
    drop(handle); // the StopHandle signals AND joins the poller thread
}

/// The RUNNING half: an update whose consumption cannot be recorded is DROPPED, not dispatched.
/// Nothing is previewed, nothing is lowered toward the core, and not one reply goes out — including
/// for a read verb, because the ledger's guarantee is about UPDATES, not about which verb one
/// happens to carry.
///
/// RED before the fix: `mark` swallowed the error, so `/status` was answered and `/cancel` minted a
/// confirmation token, both with no durable record that the update had been consumed.
#[test]
fn an_update_whose_mark_cannot_be_persisted_is_never_dispatched() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ledger = UpdateLedger::open(&ledger_paths_in(dir.path())).expect("writable at open");
    // Now make the append impossible, exactly as a remount or a revoked grant would.
    let path = dir.path().join("telegram_updates.ledger");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir_all(&path).unwrap();

    let deps = StubDeps::new();
    deps.push_batch(vec![
        update(1, CHAT, "/status"),
        update(2, CHAT, "/cancel ORDER-A"),
        update(3, STRANGER, "/status"),
    ]);
    let mut pending = PendingConfirms::default();
    let report = poll_once(&deps, &config(), &ledger, &mut pending);

    assert_eq!(report.unrecorded, vec![1, 2, 3], "every update is reported as dropped");
    assert_eq!(report.replies, 0, "not one reply — a read verb is dropped too");
    assert!(deps.replies().is_empty());
    assert!(report.previewed.is_empty(), "no confirmation token is minted");
    assert!(report.executed.is_empty());
    assert!(deps.accepted().is_empty(), "nothing reached the core");
    assert!(pending.is_empty());
    // The allowlist check never even ran — the drop happens at step 2, before step 3.
    assert!(report.ignored_unlisted.is_empty());
}

/// The MOVE itself: an install whose mark still sits at the legacy `<exe_dir>` path keeps it, so
/// relocating the ledger cannot itself cause the replay it exists to prevent.
#[test]
fn the_legacy_exe_dir_mark_migrates_instead_of_replaying() {
    let dir = tempfile::tempdir().expect("tempdir");
    let legacy = dir.path().join("bin").join("telegram_updates.log");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, "5\n").unwrap();
    let state = dir.path().join("settings").join("state");

    let ledger = UpdateLedger::open(&LedgerPaths {
        path: state.join("telegram_updates.ledger"),
        legacy: Some(legacy),
    })
    .expect("the state directory is created and writable");
    assert_eq!(ledger.last(), 5, "the pre-move mark carried over");

    // …and the redelivered backlog Telegram still holds is skipped, not re-previewed.
    let deps = StubDeps::new();
    deps.push_batch(vec![update(5, CHAT, "/cancel ORDER-A")]);
    let mut pending = PendingConfirms::default();
    let report = poll_once(&deps, &config(), &ledger, &mut pending);
    assert_eq!(report.skipped_duplicate, vec![5]);
    assert!(report.previewed.is_empty() && deps.accepted().is_empty());
    assert_eq!(deps.offsets(), vec![6], "the ACK resumes where the legacy file left off");
}

// ---------------------------------------------------------------------------------------------
// The shared acceptance path (real core + real audit trail)
// ---------------------------------------------------------------------------------------------

/// A Telegram-origin command and a TCP-origin one are gated by the SAME code over the SAME
/// `ControlLimits` bucket, and both leave the same shape of audit record. Proven by wiring this
/// channel's `preview`/`accept` hooks to the real [`accept_command`] over one shared bucket:
///
/// - a Telegram write that busts the notional cap is refused at PREVIEW with the SERVER's own
///   message, and never mints a token;
/// - the rate bucket is genuinely shared — one TCP accept plus one Telegram accept exhausts a
///   2-command budget, and the next TCP accept is rate-limited;
/// - both accepted commands appear in the audit trail, each with its own rationale.
#[test]
fn accept_command_shared_by_both_surfaces() {
    test_init();
    let mount = build_paper_maker_core(&MakerMountConfig::polymarket(TOKEN, Some(RESOLUTION_TS)));
    let sink = mount.handle.command_sink();

    // ONE bucket, both surfaces: two commands per second, nothing over $1000 notional.
    let limits = Arc::new(Mutex::new(ControlLimits::new(ControlLimitsConfig {
        max_notional: Some(1_000.0),
        rate_per_sec: 2.0,
    })));
    let preview_limits = Arc::clone(&limits);
    let accept_limits = Arc::clone(&limits);
    let accept_sink = sink.clone();
    let deps = StubDeps::new()
        .with_preview(Box::new(move |cmd| preview_limits.lock().unwrap().preview_vet(cmd)))
        .with_accept(Box::new(move |cmd, reason| {
            // `peer: None` — this surface has no socket; the origin rides in `reason`. `key_id:
            // None` for the same reason: this channel authenticates a chat id, not a node key.
            accept_command(
                cmd,
                Some(reason),
                &mut accept_limits.lock().unwrap(),
                &accept_sink,
                None,
                None,
                None,
            )
            .map(vike_tradehub::server::Accepted::into_coid)
            .map_err(|e| e.message())
        }));
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();
    let peer: std::net::SocketAddr = "127.0.0.1:65000".parse().unwrap();

    // (a) The notional cap refuses at PREVIEW — the same cap, the same message a TCP peer sees, and
    // no confirmation token is minted for a command that could never pass. Consumes no rate token.
    deps.push_batch(vec![update(1, CHAT, "/submit hyperliquid BTC buy 100 50")]);
    let report = poll_once(&deps, &config(), &led, &mut pending);
    assert!(report.previewed.is_empty(), "an over-cap command mints NO confirmation token");
    assert_eq!(report.refused.len(), 1);
    assert!(
        report.refused[0].contains("max_notional_per_order"),
        "the server's own refusal, naming the POLICY key (settings unification, Phase 5 — the old \
         VIKE_TRADEHUB_* variable no longer exists), not a Telegram-local message: {}",
        report.refused[0]
    );
    assert!(pending.is_empty());

    // (b) A within-cap command previews (still no rate token consumed) …
    deps.push_batch(vec![update(2, CHAT, "/cancel tg-a")]);
    let previewed = poll_once(&deps, &config(), &led, &mut pending);
    let token = previewed.previewed.first().cloned().expect("a token was issued");
    deps.push_batch(vec![update(3, CHAT, &format!("/confirm {token}"))]);

    // (c) … and now the three accepts run BACK-TO-BACK over the shared 2-token bucket, so the
    // refill (2/s) cannot meaningfully move between them: TCP spends token 1, Telegram spends
    // token 2, and the next TCP command is rate-limited BY THE SAME LIMITER. That is the proof the
    // two surfaces are not merely similar but literally the same gate.
    let coid = accept_command(
        WireCommand::Cancel("tcp-a".into()),
        Some("tcp: pulling the quote"),
        &mut limits.lock().unwrap(),
        &sink,
        None,
        Some(peer),
        None,
    )
    .expect("the first command is within budget")
    .into_coid();
    assert_eq!(coid, "tcp-a");

    let report = poll_once(&deps, &config(), &led, &mut pending);
    assert_eq!(report.executed, vec!["tg-a".to_string()]);

    let err = accept_command(
        WireCommand::Cancel("tcp-b".into()),
        Some("tcp: one too many"),
        &mut limits.lock().unwrap(),
        &sink,
        None,
        Some(peer),
        None,
    )
    .expect_err("the shared 2/s budget is exhausted");
    assert!(err.message().contains("rate limited"), "{}", err.message());
    assert!(!err.is_fatal(), "a rate limit never closes the surface");

    // (d) Both surfaces left the same shape of audit record.
    let tcp = audit_entry_for("tcp-a").expect("the TCP command was audited");
    assert_eq!(tcp.kind, "cancel");
    assert_eq!(tcp.reason.as_deref(), Some("tcp: pulling the quote"));
    let tg = audit_entry_for("tg-a").expect("the Telegram command was audited");
    assert_eq!(tg.kind, "cancel");
    assert!(
        tg.reason.as_deref().is_some_and(|r| r.starts_with("telegram chat ")),
        "the Telegram record names its origin (peer is None for a non-socket surface): {:?}",
        tg.reason
    );
}

/// The operator's literal instruction is what the audit trail records — that is the whole point of
/// routing the chat text into the v4 `reason` field. It reaches `audit::record` SANITIZED (control
/// characters stripped) via the shared `accept_command`, exactly as a TCP peer's rationale does.
#[test]
fn chat_text_becomes_audit_reason() {
    test_init();
    let mount = build_paper_maker_core(&MakerMountConfig::polymarket(TOKEN, Some(RESOLUTION_TS)));
    let sink = mount.handle.command_sink();
    let limits = Arc::new(Mutex::new(ControlLimits::new(ControlLimitsConfig::default())));
    let deps = StubDeps::new().with_accept(Box::new(move |cmd, reason| {
        accept_command(cmd, Some(reason), &mut limits.lock().unwrap(), &sink, None, None, None)
            .map(vike_tradehub::server::Accepted::into_coid)
            .map_err(|e| e.message())
    }));
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();

    // A rationale a human would actually type — including a newline, which the sanitizer must strip
    // before it can reach the structured JSON audit line.
    let instruction = "/cancel AUDIT-COID-1\nliquidity thinning ahead of the print";
    deps.push_batch(vec![update(1, CHAT, instruction)]);
    let previewed = poll_once(&deps, &config(), &led, &mut pending);
    let token = previewed.previewed.first().cloned().expect("a token was issued");
    deps.push_batch(vec![update(2, CHAT, &format!("/confirm {token}"))]);
    let report = poll_once(&deps, &config(), &led, &mut pending);
    assert_eq!(report.executed, vec!["AUDIT-COID-1".to_string()]);

    let entry = audit_entry_for("AUDIT-COID-1").expect("the command was audited");
    let reason = entry.reason.expect("a Telegram command always carries a rationale");
    assert!(
        reason.contains("liquidity thinning ahead of the print"),
        "the operator's literal words ARE the audit rationale: {reason}"
    );
    assert!(
        reason.contains(&format!("telegram chat {CHAT}")),
        "…and it names the origin: {reason}"
    );
    assert!(
        !reason.contains('\n') && !reason.contains('\r'),
        "the shared path sanitized it — no line terminator can reach the audit line: {reason:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// Read verbs + housekeeping
// ---------------------------------------------------------------------------------------------

/// Read verbs answer straight from the snapshot: no token, no confirm, nothing lowered. Plain
/// chatter and non-text updates are consumed silently (the offset still advances) so a bot sitting
/// in a busy group is not a noise source.
#[test]
fn read_verbs_answer_without_touching_the_core() {
    let deps = StubDeps::new();
    deps.push_batch(vec![
        update(1, CHAT, "/status"),
        update(2, CHAT, "/positions"),
        update(3, CHAT, "/orders"),
        update(4, CHAT, "/equity"),
        update(5, CHAT, "morning"),
        TgUpdate {
            update_id: 6,
            chat_id: CHAT,
            from_id: USER,
            from_username: None,
            text: String::new(),
        },
    ]);
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();
    let report = poll_once(&deps, &config(), &led, &mut pending);

    assert_eq!(report.replies, 4, "only the four read verbs are answered");
    assert!(deps.accepted().is_empty());
    assert!(report.previewed.is_empty());
    let texts: Vec<String> = deps.replies().into_iter().map(|(_, t)| t).collect();
    assert_eq!(texts, ["read:Status", "read:Positions", "read:Orders", "read:Equity"]);
    assert_eq!(led.last(), 6, "every update, answered or not, advances the offset");
}

/// An unrecognized or malformed `/verb` is answered with the usage line — and nothing else happens.
#[test]
fn unknown_and_malformed_instructions_only_get_usage() {
    let deps = StubDeps::new();
    deps.push_batch(vec![
        update(1, CHAT, "/wat"),
        update(2, CHAT, "/submit hyperliquid"),
        update(3, CHAT, "/state sideways"),
    ]);
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();
    let report = poll_once(&deps, &config(), &led, &mut pending);

    assert_eq!(report.replies, 3);
    assert!(report.previewed.is_empty() && report.executed.is_empty());
    assert!(deps.accepted().is_empty());
    for (_, text) in deps.replies() {
        assert!(text.contains("/confirm <token>"), "every answer carries the usage line: {text}");
    }
}

/// A refusal from the core (the `accept_command` error path) is reported back and the command is
/// gone — a failed confirm is never silently retried behind the operator's back.
#[test]
fn a_refused_confirm_reports_and_drops_the_command() {
    let deps =
        StubDeps::new().with_accept(Box::new(|_cmd, _reason| Err("core busy, retry".to_string())));
    deps.push_batch(vec![update(1, CHAT, "/cancel ORDER-A")]);
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();
    let previewed = poll_once(&deps, &config(), &led, &mut pending);
    let token = previewed.previewed.first().cloned().expect("a token was issued");

    deps.push_batch(vec![update(2, CHAT, &format!("/confirm {token}"))]);
    let report = poll_once(&deps, &config(), &led, &mut pending);

    assert!(report.executed.is_empty());
    assert_eq!(report.refused, vec!["core busy, retry".to_string()]);
    assert!(pending.is_empty(), "the token was spent even though the core refused");
    let last = deps.replies().last().cloned().expect("a reply").1;
    assert!(last.contains("REFUSED") && last.contains("core busy"), "{last}");
}

/// A submit reaching the shared path with an EMPTY client-order-id is refused (the remote-submit
/// idempotency policy). The Telegram grammar mints a coid at preview time and can never produce
/// one, so this pins the shared path's own guard rather than the channel's.
#[test]
fn an_empty_coid_submit_is_refused_by_the_shared_path() {
    let mount = build_paper_maker_core(&MakerMountConfig::polymarket(TOKEN, Some(RESOLUTION_TS)));
    let sink = mount.handle.command_sink();
    let mut limits = ControlLimits::new(ControlLimitsConfig::default());
    let err = accept_command(
        WireCommand::Submit(WireOrderRequest {
            client_order_id: String::new(),
            venue: "polymarket".into(),
            symbol: TOKEN.into(),
            side: 1,
            qty: 1.0,
            order_type: "limit".into(),
            price: Some(0.4),
            trigger_price: None,
            reduce_only: false,
        }),
        None,
        &mut limits,
        &sink,
        None,
        None,
        None,
    )
    .expect_err("an empty coid is refused");
    assert!(err.message().contains("pre-minted client_order_id"), "{}", err.message());
    assert!(!err.is_fatal());
}

// ---------------------------------------------------------------------------------------------
// The retry policy — permanent vs transient `getUpdates` failure
// ---------------------------------------------------------------------------------------------
//
// These two are the only tests in this file that drive the REAL poller THREAD
// (`vike_tradehub::telegram::spawn`) rather than calling `poll_once` directly, because what they
// assert is a property OF THE LOOP: how many requests a failing endpoint receives, and whether the
// loop is still there afterwards. The schedule itself (0.5s, 1s, 2s … 60s) and the announcement
// throttle are pinned exactly, without sleeping, in
// `crates/vike-tradehub/src/telegram/failure.rs`'s unit tests.

/// The scripted endpoint for the two loop tests: a `getUpdates` that can be made to fail, a poll
/// COUNTER (the request volume the retry policy exists to bound), and the replies it produced.
///
/// Deliberately NOT [`StubDeps`]. `spawn` takes ownership of its deps, so observing one afterwards
/// means sharing it through an `Arc` — and `Arc<T>` is `Send` only when `T: Sync`, which `StubDeps`
/// is not (its `preview_fn`/`accept_fn` are `Box<dyn Fn … + Send>`). These two tests need none of
/// that machinery: nothing here previews, confirms or reaches a core.
#[derive(Default)]
struct PollStub {
    /// While `Some`, EVERY `get_updates` fails with this instead of serving a batch.
    fail: Mutex<Option<PollError>>,
    batches: Mutex<VecDeque<Vec<TgUpdate>>>,
    /// One per `get_updates` call — the REQUEST count.
    polls: AtomicUsize,
    replies: Mutex<Vec<String>>,
}

impl PollStub {
    fn failing_with(e: PollError) -> Arc<Self> {
        let s = Arc::new(PollStub::default());
        *s.fail.lock().unwrap() = Some(e);
        s
    }
    /// End the scripted outage: subsequent polls serve batches again.
    fn heal(&self) {
        *self.fail.lock().unwrap() = None;
    }
    fn push_batch(&self, updates: Vec<TgUpdate>) {
        self.batches.lock().unwrap().push_back(updates);
    }
    fn polls(&self) -> usize {
        self.polls.load(Ordering::Relaxed)
    }
    fn replies(&self) -> usize {
        self.replies.lock().unwrap().len()
    }
}

/// The `Box<dyn TelegramDeps + Send>` view onto a shared [`PollStub`]. (A blanket
/// `impl TelegramDeps for Arc<PollStub>` is not available — `Arc` is not `#[fundamental]`, so the
/// orphan rule rejects it — hence the newtype.)
struct SharedDeps(Arc<PollStub>);

impl TelegramDeps for SharedDeps {
    fn get_updates(&self, _offset: i64) -> Result<Vec<TgUpdate>, PollError> {
        self.0.polls.fetch_add(1, Ordering::Relaxed);
        if let Some(e) = self.0.fail.lock().unwrap().clone() {
            return Err(e);
        }
        Ok(self.0.batches.lock().unwrap().pop_front().unwrap_or_default())
    }
    fn send_message(&self, _chat_id: i64, text: &str) {
        self.0.replies.lock().unwrap().push(text.to_string());
    }
    fn preview(&self, _cmd: &WireCommand) -> Option<String> {
        None
    }
    fn accept(&self, _cmd: WireCommand, _reason: &str) -> Result<String, String> {
        unreachable!("these tests never confirm anything")
    }
    fn read(&self, verb: ReadVerb) -> String {
        format!("read:{verb:?}")
    }
    fn mint_coid(&self) -> String {
        "tg-coid".to_string()
    }
    fn mint_token(&self) -> String {
        "tok".to_string()
    }
    fn now_ms(&self) -> i64 {
        0
    }
}

/// Poll `cond` until it holds or `deadline` elapses. Generous deadlines everywhere below: these
/// tests assert BOUNDS (a count that must not grow, an answer that must eventually arrive), never
/// that something happened by a particular millisecond, so a loaded CI box makes them slower rather
/// than red.
fn wait_until(deadline: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let until = std::time::Instant::now() + deadline;
    while std::time::Instant::now() < until {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    cond()
}

/// ⚠ **THE DEFECT.** `getUpdates` with a wrong bot token answers `404` INSTANTLY — there is no 20 s
/// long-poll to wait through — so a loop that answers every failure with `POLL_GAP` and another
/// attempt runs at ~2 requests/second. Measured on a clean install with a mistyped
/// `VIKE_TELEGRAM_BOT_TOKEN`: 19 WARN lines in 10 s, i.e. ~164,000 requests to `api.telegram.org`
/// and ~164,000 WARN lines (~37 MB) per day — at `warn`, the level the deploy runbook tells
/// operators to run the file layer at, so the recommended configuration could not turn it down.
///
/// A 404 is a rejected CREDENTIAL: it cannot start working by being asked again. So the channel
/// STOPS, which is `maybe_spawn`'s own "a control channel whose precondition cannot be met does not
/// exist" refusal, fired at the first pass that carries the evidence.
#[test]
fn a_permanent_getupdates_failure_stops_the_poller_instead_of_retrying_forever() {
    let stub = PollStub::failing_with(PollError::from_status(404));
    let (_dir, led) = ledger();
    let handle = spawn(config(), led, Box::new(SharedDeps(stub.clone())));

    assert!(
        wait_until(Duration::from_secs(10), || stub.polls() >= 1),
        "the poller must make its first request"
    );
    // Three times the old 500 ms cadence, so the old loop would be at 3-4 requests by now.
    std::thread::sleep(Duration::from_millis(1_500));
    assert_eq!(
        stub.polls(),
        1,
        "a rejected bot token must cost exactly ONE request and ONE log line, ever — the old loop \
         kept asking ~2x/second forever"
    );

    handle.shutdown(); // the thread already ended; shutdown/Drop still joins cleanly
}

/// The other half, which the fix must NOT buy the first half with: a 5xx, a rate limit or a dead
/// socket is the endpoint's problem, not the credential's, so the channel keeps retrying and is
/// still there when the endpoint comes back.
#[test]
fn a_transient_getupdates_failure_keeps_the_channel_alive_and_recovers() {
    let stub = PollStub::failing_with(PollError::from_status(503));
    let (_dir, led) = ledger();
    let handle = spawn(config(), led, Box::new(SharedDeps(stub.clone())));

    assert!(
        wait_until(Duration::from_secs(10), || stub.polls() >= 2),
        "a transient failure must be RETRIED — stopping the channel on a 503 would make one bad \
         minute at Telegram cost an operator their remote control until they noticed and restarted"
    );

    // …and the channel answers again once the endpoint returns. (Queue the batch BEFORE healing so
    // the first reachable poll already has something to serve.)
    stub.push_batch(vec![update(1, CHAT, "/status")]);
    stub.heal();
    assert!(
        wait_until(Duration::from_secs(30), || stub.replies() > 0),
        "the poller must still be running and must answer once getUpdates succeeds again"
    );

    handle.shutdown();
}

// ---------------------------------------------------------------------------------------------
// Audit capture (the `control_roundtrip.rs` idiom)
// ---------------------------------------------------------------------------------------------

/// Observing the AUDIT trail from an integration test.
///
/// `audit::record` emits ONE `tracing::info!` event, and `tracing::subscriber::with_default` is a
/// THREAD-LOCAL dispatcher, so the capture has to be the process-global subscriber.
/// `tracing-subscriber` is not a dependency of this crate, so this is a minimal hand-rolled
/// [`Subscriber`] that is `enabled` ONLY for the `vike_tradehub::audit` target and appends each
/// event's `(kind, coid, reason)` to a shared buffer. Every test that needs it calls [`test_init`]
/// (in place of `vike_log::test_init`) so the install cannot lose the one-global-subscriber race.
mod audit_capture {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Mutex, Once, OnceLock};

    use tracing::field::{Field, Visit};
    use tracing::{span, Event, Metadata, Subscriber};

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

    fn captured() -> &'static Mutex<Vec<AuditEntry>> {
        static LOG: OnceLock<Mutex<Vec<AuditEntry>>> = OnceLock::new();
        LOG.get_or_init(|| Mutex::new(Vec::new()))
    }

    /// Install the capture subscriber exactly once for this test binary. Best-effort by design: a
    /// failed install makes the capture EMPTY, which the audit tests then fail on loudly rather
    /// than passing vacuously.
    pub fn test_init() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            let _ = tracing::subscriber::set_global_default(AuditCapture {
                next_span: AtomicU64::new(1), // span::Id::from_u64 panics on 0
            });
        });
    }

    /// The audit entry recorded for `coid`, waited on briefly (`audit::record` runs on whichever
    /// thread accepted the command, so this is belt-and-braces rather than a race the assertion
    /// depends on).
    pub fn audit_entry_for(coid: &str) -> Option<AuditEntry> {
        for _ in 0..200 {
            if let Some(e) =
                captured().lock().expect("audit capture poisoned").iter().find(|e| e.coid == coid)
            {
                return Some(e.clone());
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        None
    }

    struct AuditCapture {
        next_span: AtomicU64,
    }

    impl Subscriber for AuditCapture {
        /// ONLY the audit target — everything else in the process is dropped at the callsite.
        fn enabled(&self, metadata: &Metadata<'_>) -> bool {
            metadata.target() == AUDIT_TARGET
        }
        fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
            span::Id::from_u64(self.next_span.fetch_add(1, Ordering::Relaxed))
        }
        fn record(&self, _: &span::Id, _: &span::Record<'_>) {}
        fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}
        fn event(&self, event: &Event<'_>) {
            let mut visitor = FieldVisitor::default();
            event.record(&mut visitor);
            captured().lock().expect("audit capture poisoned").push(AuditEntry {
                kind: visitor.kind,
                coid: visitor.coid,
                reason: visitor.reason,
            });
        }
        fn enter(&self, _: &span::Id) {}
        fn exit(&self, _: &span::Id) {}
    }

    /// Pull the three string fields off the audit event. `reason` is recorded as `Option<&str>`,
    /// which tracing records as the inner `&str` when `Some` and emits NO field at all when `None`.
    #[derive(Default)]
    struct FieldVisitor {
        kind: String,
        coid: String,
        reason: Option<String>,
    }

    impl Visit for FieldVisitor {
        fn record_str(&mut self, field: &Field, value: &str) {
            match field.name() {
                "kind" => self.kind = value.to_string(),
                "coid" => self.coid = value.to_string(),
                "reason" => self.reason = Some(value.to_string()),
                _ => {}
            }
        }
        // The `message` / `?peer` fields arrive here; nothing to capture from them.
        fn record_debug(&mut self, _: &Field, _: &dyn std::fmt::Debug) {}
    }
}
