//! The scripted deps: the stub `TelegramDeps`, the update and config builders, the ledgers.

use super::*;

// ---------------------------------------------------------------------------------------------
// The scripted deps
// ---------------------------------------------------------------------------------------------

pub(super) type PreviewFn = Box<dyn Fn(&WireCommand) -> Option<String> + Send>;
pub(super) type AcceptFn = Box<dyn Fn(WireCommand, &str) -> Result<String, String> + Send>;

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
pub(super) struct StubDeps {
    batches: Mutex<VecDeque<Vec<TgUpdate>>>,
    rec: Mutex<Recorded>,
    now: AtomicI64,
    coid_seq: AtomicUsize,
    token_seq: AtomicUsize,
    preview_fn: Option<PreviewFn>,
    accept_fn: Option<AcceptFn>,
}

impl StubDeps {
    pub(super) fn new() -> Self {
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
    pub(super) fn push_batch(&self, updates: Vec<TgUpdate>) {
        self.batches.lock().unwrap().push_back(updates);
    }

    pub(super) fn set_now(&self, ms: i64) {
        self.now.store(ms, Ordering::Relaxed);
    }

    pub(super) fn with_preview(mut self, f: PreviewFn) -> Self {
        self.preview_fn = Some(f);
        self
    }

    pub(super) fn with_accept(mut self, f: AcceptFn) -> Self {
        self.accept_fn = Some(f);
        self
    }

    pub(super) fn accepted(&self) -> Vec<(WireCommand, String)> {
        self.rec.lock().unwrap().accepted.clone()
    }

    pub(super) fn replies(&self) -> Vec<(i64, String)> {
        self.rec.lock().unwrap().replies.clone()
    }

    pub(super) fn offsets(&self) -> Vec<i64> {
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
/// `server::control::lower_command` would echo).
fn coid_of(cmd: &WireCommand) -> String {
    match cmd {
        WireCommand::Submit(r) => r.client_order_id.clone(),
        WireCommand::Cancel(c) => c.clone(),
        WireCommand::Modify { client_order_id, .. } => client_order_id.clone(),
        _ => String::new(),
    }
}

/// The default sender for the harness — one operator, the DM shape.
pub(super) const USER: i64 = 55;
/// A SECOND sender in the same allowlisted chat: the group shape, and the case a chat-only
/// allowlist cannot distinguish at all.
pub(super) const OTHER_USER: i64 = 66;

pub(super) fn update(id: i64, chat: i64, text: &str) -> TgUpdate {
    update_from(id, chat, USER, text)
}

/// An update from a NAMED sender — the group case.
pub(super) fn update_from(id: i64, chat: i64, from_id: i64, text: &str) -> TgUpdate {
    TgUpdate {
        update_id: id,
        chat_id: chat,
        from_id,
        from_username: Some(format!("u{from_id}")),
        text: text.to_string(),
    }
}

/// A config allowlisting exactly [`CHAT`], with NO user allowlist — the default, chat-only shape.
pub(super) fn config() -> TelegramConfig {
    TelegramConfig::new("123456:test-token", vec![CHAT]).expect("a token + one chat configures it")
}

/// The same config TIGHTENED to a single user — the opt-in a group deployment can turn on.
pub(super) fn config_user_allowlisted() -> TelegramConfig {
    config().allowing_users(vec![USER])
}

/// A fresh ledger in a throwaway directory. The `TempDir` is returned so the caller keeps it alive.
pub(super) fn ledger() -> (tempfile::TempDir, UpdateLedger) {
    let dir = tempfile::tempdir().expect("tempdir");
    let ledger = UpdateLedger::open(&ledger_paths_in(dir.path())).expect("a writable temp dir");
    (dir, ledger)
}

/// [`LedgerPaths`] under `dir`, with no legacy half — the steady state after the move.
pub(super) fn ledger_paths_in(dir: &Path) -> LedgerPaths {
    LedgerPaths { path: dir.join("telegram_updates.ledger"), legacy: None }
}

/// A `LedgerPaths` pointing INSIDE a plain file, so neither the directory nor the ledger can be
/// created. The portable stand-in for the read-only `<exe_dir>` this channel used to write into
/// (`crates/vike-model/src/paths/state_path/tests/state_and_log.rs`'s `an_uncreatable_state_dir_errors_instead_of_panicking`
/// uses the same trick; a mode-000 directory does not work on the Windows dev box).
pub(super) fn unwritable_ledger_paths(dir: &Path) -> LedgerPaths {
    let blocker = dir.join("blocker");
    std::fs::write(&blocker, "not a directory").expect("blocker");
    LedgerPaths { path: blocker.join("state").join("telegram_updates.ledger"), legacy: None }
}
