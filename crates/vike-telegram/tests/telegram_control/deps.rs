//! The scripted deps: the stub `TelegramDependencies`, the update and config builders, the ledgers.

use super::*;

// ---------------------------------------------------------------------------------------------
// The scripted deps
// ---------------------------------------------------------------------------------------------

pub(super) type PreviewFn = Box<dyn Fn(&CommandWire) -> Option<NotExecuted> + Send>;
pub(super) type AcceptFn = Box<dyn Fn(CommandWire, &str) -> Outcome + Send>;

/// Everything the stub observed, so a test can assert on what the channel DID rather than only on
/// what it reported.
#[derive(Default)]
struct Recorded {
    /// `offset` passed to each `getUpdates` — the Telegram ACK, and the restart-replay guard.
    offsets: Vec<i64>,
    /// every reply actually sent, with its target chat.
    replies: Vec<(i64, String)>,
    /// every command that reached `accept` — i.e. everything that could move an order.
    accepted: Vec<(CommandWire, String)>,
}

/// The scripted [`TelegramDependencies`]: canned update batches, an injected clock, deterministic nonces,
/// and pluggable preview/accept hooks.
pub(super) struct StubDependencies {
    batches: Mutex<VecDeque<Vec<TelegramUpdate>>>,
    rec: Mutex<Recorded>,
    now: AtomicI64,
    order_client_id_sequence: AtomicUsize,
    token_sequence: AtomicUsize,
    preview_fn: Option<PreviewFn>,
    preview_notes: Vec<String>,
    accept_fn: Option<AcceptFn>,
}

impl StubDependencies {
    pub(super) fn new() -> Self {
        StubDependencies {
            batches: Mutex::new(VecDeque::new()),
            rec: Mutex::new(Recorded::default()),
            now: AtomicI64::new(0),
            order_client_id_sequence: AtomicUsize::new(0),
            token_sequence: AtomicUsize::new(0),
            preview_fn: None,
            preview_notes: Vec::new(),
            accept_fn: None,
        }
    }

    pub(super) fn with_preview_notes(mut self, notes: Vec<String>) -> Self {
        self.preview_notes = notes;
        self
    }

    /// Queue one `getUpdates` batch (one per [`poll_once`] call, in order).
    pub(super) fn push_batch(&self, updates: Vec<TelegramUpdate>) {
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

    pub(super) fn accepted(&self) -> Vec<(CommandWire, String)> {
        self.rec.lock().unwrap().accepted.clone()
    }

    pub(super) fn replies(&self) -> Vec<(i64, String)> {
        self.rec.lock().unwrap().replies.clone()
    }

    pub(super) fn offsets(&self) -> Vec<i64> {
        self.rec.lock().unwrap().offsets.clone()
    }
}

impl TelegramDependencies for StubDependencies {
    fn get_updates(&self, offset: i64) -> Result<Vec<TelegramUpdate>, PollError> {
        self.rec.lock().unwrap().offsets.push(offset);
        Ok(self.batches.lock().unwrap().pop_front().unwrap_or_default())
    }

    fn send_message(&self, chat_id: i64, text: &str) {
        self.rec.lock().unwrap().replies.push((chat_id, text.to_string()));
    }

    fn preview(&self, cmd: &CommandWire) -> Result<Vec<String>, NotExecuted> {
        match self.preview_fn.as_ref().and_then(|f| f(cmd)) {
            Some(refusal) => Err(refusal),
            None => Ok(self.preview_notes.clone()), // default: the node would pass it
        }
    }

    fn accept(&self, cmd: CommandWire, reason: &str) -> Outcome {
        self.rec.lock().unwrap().accepted.push((cmd.clone(), reason.to_string()));
        match &self.accept_fn {
            Some(f) => f(cmd, reason),
            None => Outcome::Sent { order_client_id: order_client_id_of(&cmd), notes: Vec::new() },
        }
    }

    fn read(&self, verb: ReadVerb) -> String {
        format!("read:{verb:?}")
    }

    /// A deterministic id, minted once per write preview as the production mint is.
    fn fill_order_client_id(&self, cmd: CommandWire) -> CommandWire {
        let order_client_id = format!(
            "tg-order-client-id-{}",
            self.order_client_id_sequence.fetch_add(1, Ordering::Relaxed)
        );
        match cmd {
            CommandWire::Submit(mut r) if r.order_client_id.is_empty() => {
                r.order_client_id = order_client_id;
                CommandWire::Submit(r)
            }
            other => other,
        }
    }

    fn mint_token(&self) -> String {
        format!("tok{}", self.token_sequence.fetch_add(1, Ordering::Relaxed))
    }

    fn now_ms(&self) -> i64 {
        self.now.load(Ordering::Relaxed)
    }
}

/// The order client id a command targets, for the stub's default `accept` answer (what the node echoes).
fn order_client_id_of(cmd: &CommandWire) -> String {
    match cmd {
        CommandWire::Submit(r) => r.order_client_id.clone(),
        CommandWire::Cancel(c) => c.clone(),
        CommandWire::Modify { order_client_id, .. } => order_client_id.clone(),
        _ => String::new(),
    }
}

/// The default sender for the harness — one operator, the DM shape.
pub(super) const USER: i64 = 55;
/// A SECOND sender in the same allowlisted chat: the group shape, and the case a chat-only
/// allowlist cannot distinguish at all.
pub(super) const OTHER_USER: i64 = 66;

pub(super) fn update(id: i64, chat: i64, text: &str) -> TelegramUpdate {
    update_from(id, chat, USER, text)
}

/// An update from a NAMED sender — the group case.
pub(super) fn update_from(id: i64, chat: i64, from_id: i64, text: &str) -> TelegramUpdate {
    TelegramUpdate {
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
    let ledger = UpdateLedger::open(&ledger_path_in(dir.path())).expect("a writable temp dir");
    (dir, ledger)
}

/// The ledger file under `dir`.
pub(super) fn ledger_path_in(dir: &Path) -> PathBuf {
    dir.join(vike_telegram::telegram_cli::TELEGRAM_LEDGER_FILE)
}

/// A ledger path pointing INSIDE a plain file, so neither the directory nor the ledger can be
/// created. The portable stand-in for the read-only `<exe_dir>` this channel used to write into
/// (`crates/vike-paths/src/state_path/tests/state_and_log.rs`'s `an_uncreatable_state_dir_errors_instead_of_panicking`
/// uses the same trick; a mode-000 directory does not work on the Windows dev box).
pub(super) fn unwritable_ledger_path(dir: &Path) -> PathBuf {
    let blocker = dir.join("blocker");
    std::fs::write(&blocker, "not a directory").expect("blocker");
    blocker.join("state").join(vike_telegram::telegram_cli::TELEGRAM_LEDGER_FILE)
}
