//! The desktop's client of the node's CREDENTIAL verbs, for the two DATA-provider keys the Data
//! Manager's "Data providers" group edits (`vike_venues::credential_keys::DATA_PROVIDER_KEYS`:
//! Finnhub for earnings, FMP for dividends).
//!
//! The keys are the SERVER's, not this PC's: they land in the credential store on the node the
//! desktop is connected to, where the datahub's tool-feeds lane re-reads them every cycle
//! (`docs/decisions/0143-the-news-calendar-and-equity-tools-read-the-datahub-and-the-desktop-fetches-nothing.md`).
//! So the three acts are wire verbs of the account plane
//! (`docs/decisions/0065-accounts-are-managed-and-the-barrier-is-declared.md`), and every one of
//! them needs the server's ADMIN key:
//!
//! * [`check`] — a PRESENT-NAMES read (`AccountVerb::HeldCredentials`): which of the two names the
//!   node's store holds. Names only; no verb on this wire returns a value, and none can.
//! * [`save`] — `AccountVerb::SetCredential`, with the confirm the wire requires (the key's own name)
//!   filled by this module. Typing the value and pressing Save is the deliberate act.
//! * [`remove`] — `AccountVerb::RetireCredential`: the node marks the row retired and keeps it at
//!   rest; the name reads as not set from then on. The screen arms it behind a two-click.
//!
//! # ⚠ The admin key is TYPED, per session, and never stored here
//!
//! The desktop's hub-key resolution reads the observe/control pair and never the admin key
//! (`vike_venues::credential_keys::is_tradehub_pair_key`'s doc: the key that places orders is not the
//! key that writes key material). This module keeps that: the operator types the admin key into a
//! masked field, it lives in [`ProviderKeysView`] for the session, and it is never written to a
//! file, a setting, the process environment, a log or a `Debug`. Persisting it on the PC would be
//! decision 0065's reopener ("a desktop that carries the key that writes key material"), and is the
//! owner's to rule.
//!
//! # ⚠ Refused by name, client-side, against a server that cannot answer
//!
//! [`vike_tradehub_client::exchange_once`] is handed BOTH capability strings the verbs owe
//! (`account-verbs` and `credential-admin`); a node that advertises fewer is sent NOTHING after the
//! handshake, and the screen says the server does not offer credential administration. No value and
//! no admin key reaches a log line, an error text or a `Debug` of this module.

use std::sync::{Arc, Mutex, PoisonError};

use vike_hub_proto::auth::Scope;
use vike_tradehub_client::proto::{
    FEATURE_ACCOUNT_VERBS, FEATURE_CREDENTIAL_ADMIN, Request, Response,
};
use vike_tradehub_client::wire::{AccountRequest, AccountVerb};
use vike_venues::credential_keys::{FINNHUB_API_KEY, FMP_API_KEY};

/// One data-provider key as the screen shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderKey {
    /// The credential-store name — `vike_venues::credential_keys`' own constant.
    pub name: &'static str,
    /// The provider, as the row's title.
    pub label: &'static str,
    /// What the key serves, as the row's second line.
    pub serves: &'static str,
}

/// The two keys, in screen order. The names are `vike_venues::credential_keys::DATA_PROVIDER_KEYS`
/// (`the_screen_rows_are_the_closed_pair_the_wire_admits` holds the two equal).
pub const PROVIDER_KEYS: [ProviderKey; 2] = [
    ProviderKey { name: FINNHUB_API_KEY, label: "Finnhub key", serves: "earnings calendar" },
    ProviderKey { name: FMP_API_KEY, label: "FMP key", serves: "dividends calendar" },
];

/// Both capabilities every verb here owes (see the module doc).
const NEEDS: [&str; 2] = [FEATURE_ACCOUNT_VERBS, FEATURE_CREDENTIAL_ADMIN];

/// Send one account request to `addr` under the admin scope and return the node's reply, with every
/// failure worded for the operator: a refusal the node ANSWERED is its own text; a transport,
/// handshake or negotiation failure is worded here. ⚠ Nothing here formats the request (its `Debug`
/// redacts a value, but a message built from it would not be this module's to vouch for).
fn ask(
    addr: &str,
    admin_key: &str,
    verb: AccountVerb,
    confirm: Option<String>,
) -> Result<Response, String> {
    let admin_key = admin_key.trim();
    if admin_key.is_empty() {
        return Err("enter the server's admin key first — it is typed per session and never \
                    stored on this PC"
            .to_string());
    }
    let request = Request::Account(Box::new(AccountRequest { verb, confirm }));
    match vike_tradehub_client::exchange_once(
        addr,
        admin_key.as_bytes(),
        Scope::Account,
        &NEEDS,
        &request,
    ) {
        Ok(Response::Error(refusal)) => Err(refusal),
        Ok(reply) => Ok(reply),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Err(format!(
            "the server refused the admin key ({e}). Credential administration needs the \
             server's ADMIN key, and the server must have DECLARED its account-administration \
             barrier (an operator act on that box)."
        )),
        Err(e) if e.kind() == std::io::ErrorKind::Unsupported => Err(format!(
            "this server does not offer credential administration — it predates it, or its \
             account verbs are not armed. {e}"
        )),
        Err(e) => Err(format!("could not reach the server: {e}")),
    }
}

/// Which of [`PROVIDER_KEYS`] the node's store holds — the NAMES, never a value.
///
/// # Errors
/// The operator-worded reason the node could not be asked or refused.
pub fn check(addr: &str, admin_key: &str) -> Result<Vec<String>, String> {
    let names = PROVIDER_KEYS.iter().map(|k| k.name.to_string()).collect();
    match ask(addr, admin_key, AccountVerb::HeldCredentials { names }, None)? {
        Response::CredentialsHeld(held) => Ok(held.held),
        _ => Err(UNEXPECTED_REPLY.to_string()),
    }
}

/// Write `value` as the credential `key` on the node. The wire's `confirm` is the key's own name,
/// filled here: pressing Save with a typed value is the deliberate act.
///
/// # Errors
/// The operator-worded reason the node could not be asked or refused. ⚠ Never quotes `value`.
pub fn save(addr: &str, admin_key: &str, key: &str, value: &str) -> Result<String, String> {
    let verb = AccountVerb::SetCredential { key: key.to_string(), value: value.to_string() };
    written(ask(addr, admin_key, verb, Some(key.to_string()))?)
}

/// Retire the credential `key` on the node: kept at rest, answered as not set from then on.
///
/// # Errors
/// The operator-worded reason the node could not be asked or refused.
pub fn remove(addr: &str, admin_key: &str, key: &str) -> Result<String, String> {
    let verb = AccountVerb::RetireCredential { key: key.to_string() };
    written(ask(addr, admin_key, verb, Some(key.to_string()))?)
}

const UNEXPECTED_REPLY: &str = "the server answered with something other than the reply this \
                                verb expects — it may be a different version of vike-tradehub";

/// The node's note out of an accepted write.
fn written(reply: Response) -> Result<String, String> {
    match reply {
        Response::AccountWritten(done) => Ok(done.note.unwrap_or_default()),
        _ => Err(UNEXPECTED_REPLY.to_string()),
    }
}

/// What the screen knows about one key on the node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// The node's store holds it.
    Set,
    /// The node answered and does not hold it.
    NotSet,
    /// Not asked yet, or the last ask failed.
    Unknown,
}

/// The result of the one job a [`ProviderKeysView`] may have in flight.
enum Slot {
    Idle,
    Running,
    Done(Done),
}

/// A finished job, by which key (an index into [`PROVIDER_KEYS`]) it was about.
enum Done {
    Checked(Result<Vec<String>, String>),
    Saved(usize, Result<String, String>),
    Removed(usize, Result<String, String>),
}

/// The "Data providers" group's per-window state: the admin key as typed (masked, this session),
/// one value buffer per key, the two-click Remove's arm, what the node last said it holds, and the
/// one background job in flight.
///
/// ⚠ **`Debug` is HAND-WRITTEN and prints no field that can hold a secret** — the admin key and the
/// value buffers are the two places one can sit, and a derived `Debug` would put them in every
/// `{:?}` and every test panic.
#[derive(Clone)]
pub struct ProviderKeysView {
    /// The server's admin key as typed. Masked on screen; kept only for the session; never saved.
    pub admin_key: String,
    /// One value buffer per [`PROVIDER_KEYS`] row. Taken (emptied) the moment a Save is sent, so a
    /// value does not linger in UI memory after it has gone.
    pub values: [String; 2],
    /// The armed Remove: `(row index, egui time of the arming click)`. See [`Self::tick_arm`].
    pub armed: Option<(usize, f64)>,
    /// The names the node last said it holds; `None` until a check has answered.
    held: Option<Vec<String>>,
    /// The last act's outcome for the status line: `(is_error, text)`.
    status: Option<(bool, String)>,
    /// The backend address `held` was read from; a switch to another backend forgets the marks.
    addr_seen: String,
    job: Arc<Mutex<Slot>>,
}

impl Default for ProviderKeysView {
    fn default() -> Self {
        Self {
            admin_key: String::new(),
            values: [String::new(), String::new()],
            armed: None,
            held: None,
            status: None,
            addr_seen: String::new(),
            job: Arc::new(Mutex::new(Slot::Idle)),
        }
    }
}

impl std::fmt::Debug for ProviderKeysView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderKeysView")
            .field("admin_key", &if self.admin_key.is_empty() { "<empty>" } else { "<set>" })
            .field("armed", &self.armed.as_ref().map(|(i, _)| i))
            .field("held", &self.held)
            .field("busy", &self.busy())
            .finish_non_exhaustive()
    }
}

impl ProviderKeysView {
    /// How long an armed Remove waits for its second click.
    pub const ARM_WINDOW: std::time::Duration = std::time::Duration::from_secs(5);

    /// Is a job in flight? The screen disables its buttons meanwhile (one job at a time).
    pub fn busy(&self) -> bool {
        matches!(*self.slot(), Slot::Running)
    }

    /// What the node last said about row `index`.
    pub fn mark(&self, index: usize) -> Mark {
        match (&self.held, PROVIDER_KEYS.get(index)) {
            (Some(held), Some(key)) if held.iter().any(|h| h == key.name) => Mark::Set,
            (Some(_), Some(_)) => Mark::NotSet,
            _ => Mark::Unknown,
        }
    }

    /// The last act's outcome: `(is_error, text)`.
    pub fn status(&self) -> Option<&(bool, String)> {
        self.status.as_ref()
    }

    /// Expire an armed Remove whose [`Self::ARM_WINDOW`] ran out at egui time `now`. Returns the
    /// seconds still left when it is still armed (so the caller can repaint at the deadline).
    pub fn tick_arm(&mut self, now: f64) -> Option<f64> {
        let (_, armed_at) = self.armed?;
        let left = Self::ARM_WINDOW.as_secs_f64() - (now - armed_at);
        if left <= 0.0 {
            self.armed = None;
            None
        } else {
            Some(left)
        }
    }

    fn slot(&self) -> std::sync::MutexGuard<'_, Slot> {
        self.job.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Fold a finished job into the view. Called once a frame, BEFORE drawing, with the backend
    /// address the frame is about: a different address forgets what another box said.
    pub fn poll(&mut self, addr: Option<&str>, ctx: &egui::Context) {
        let addr = addr.unwrap_or_default();
        if addr != self.addr_seen {
            self.addr_seen = addr.to_string();
            self.held = None;
            self.status = None;
            self.armed = None;
        }
        let done = {
            let mut slot = self.slot();
            match std::mem::replace(&mut *slot, Slot::Idle) {
                Slot::Done(done) => Some(done),
                other => {
                    *slot = other;
                    None
                }
            }
        };
        if let Some(done) = done {
            self.apply(done, addr, ctx);
        }
    }

    /// Words the outcome and, after a write that landed, asks the node what it holds now.
    fn apply(&mut self, done: Done, addr: &str, ctx: &egui::Context) {
        match done {
            Done::Checked(Ok(held)) => self.held = Some(held),
            Done::Checked(Err(e)) => {
                self.held = None;
                self.status = Some((true, e));
            }
            Done::Saved(i, Ok(_)) => {
                self.status = Some((
                    false,
                    format!(
                        "{} saved on the server. The datahub re-reads it each cycle (about a \
                         minute); no restart is needed.",
                        PROVIDER_KEYS[i].name
                    ),
                ));
                self.refresh(addr, ctx);
            }
            Done::Removed(i, Ok(_)) => {
                self.status = Some((
                    false,
                    format!(
                        "Removed {} — kept at rest, Save a value to add it back",
                        PROVIDER_KEYS[i].name
                    ),
                ));
                self.refresh(addr, ctx);
            }
            Done::Saved(_, Err(e)) | Done::Removed(_, Err(e)) => self.status = Some((true, e)),
        }
    }

    /// Ask the node which keys it holds. A no-op while a job is in flight.
    pub fn start_check(&mut self, addr: &str, ctx: &egui::Context) {
        if !self.busy() {
            self.status = None;
        }
        self.refresh(addr, ctx);
    }

    /// [`Self::start_check`] without touching the status line: the re-read after a write that
    /// landed, which must not wipe the sentence that write just earned.
    fn refresh(&mut self, addr: &str, ctx: &egui::Context) {
        let key = self.admin_key.clone();
        let addr = addr.to_string();
        self.spawn(ctx, move || Done::Checked(check(&addr, &key)));
    }

    /// Send row `index`'s typed value. The buffer is TAKEN: it is empty from this point on.
    pub fn start_save(&mut self, index: usize, addr: &str, ctx: &egui::Context) {
        if self.busy() {
            return;
        }
        let value = std::mem::take(&mut self.values[index]);
        if value.trim().is_empty() {
            self.status = Some((true, "nothing entered — no changes saved".to_string()));
            return;
        }
        self.status = None;
        let key = self.admin_key.clone();
        let addr = addr.to_string();
        let name = PROVIDER_KEYS[index].name;
        self.spawn(ctx, move || Done::Saved(index, save(&addr, &key, name, value.trim())));
    }

    /// Send row `index`'s Remove (the second click).
    pub fn start_remove(&mut self, index: usize, addr: &str, ctx: &egui::Context) {
        self.armed = None;
        self.status = None;
        let key = self.admin_key.clone();
        let addr = addr.to_string();
        let name = PROVIDER_KEYS[index].name;
        self.spawn(ctx, move || Done::Removed(index, remove(&addr, &key, name)));
    }

    /// Run `work` on its own thread and leave its result in the slot, waking the UI. One job at a
    /// time: asking while one runs does nothing.
    fn spawn(&mut self, ctx: &egui::Context, work: impl FnOnce() -> Done + Send + 'static) {
        {
            let mut slot = self.slot();
            if matches!(*slot, Slot::Running) {
                return;
            }
            *slot = Slot::Running;
        }
        let job = Arc::clone(&self.job);
        let ctx = ctx.clone();
        let spawned =
            std::thread::Builder::new().name("vt-provider-keys".into()).spawn(move || {
                let done = work();
                *job.lock().unwrap_or_else(PoisonError::into_inner) = Slot::Done(done);
                ctx.request_repaint();
            });
        if let Err(e) = spawned {
            *self.slot() = Slot::Idle;
            self.status = Some((true, format!("could not start the request: {e}")));
        }
    }

    /// TEST DOOR: leave a finished job in the slot, as the worker thread would.
    #[cfg(test)]
    fn finish(&self, done: Done) {
        *self.slot() = Slot::Done(done);
    }
}

#[path = "provider_keys_tests.rs"]
#[cfg(test)]
mod provider_keys_tests;
