//! The desktop's client of the node's CREDENTIAL verbs, for the two DATA-provider keys the Data
//! Manager's "Data providers" group edits (`vike_venues::credential_keys::DATA_PROVIDER_KEYS`:
//! Finnhub for earnings, FMP for dividends).
//!
//! The keys are the SERVER's, not this PC's: they land in the credential store on the node the
//! desktop is connected to, where the datahub's tool-feeds lane re-reads them every cycle
//! (`docs/decisions/0143-the-news-calendar-and-equity-tools-read-the-datahub-and-the-desktop-fetches-nothing.md`).
//! So the three acts are wire verbs of the account plane
//! (`docs/decisions/0065-accounts-are-managed-and-the-barrier-is-declared.md`), and every one of
//! them needs this desktop's device to hold the `admin` role on the server's access list:
//!
//! * [`check`] — a PRESENT-NAMES read (`AccountVerbWire::HeldCredentials`): which of the two names the
//!   node's store holds. Names only; no verb on this wire returns a value, and none can.
//! * [`save`] — `AccountVerbWire::SetCredential`, with the confirm the wire requires (the key's own name)
//!   filled by this module. Typing the value and pressing Save is the deliberate act.
//! * [`remove`] — `AccountVerbWire::RetireCredential`: the node marks the row retired and keeps it at
//!   rest; the name reads as not set from then on. The screen arms it behind a two-click.
//!
//! # ⚠ This desktop's device key signs, and admin is its ROLE; no admin key exists here
//!
//! Since decision 0288 admin is a ROLE of a device on the server's access list: every act is signed
//! with this desktop's own device key (`crate::backend::registry::device_key`), and the server's
//! list decides. No person holds or types an admin key (decision 0288 item 8), so the desktop carries
//! no key that writes key material (decision 0065's reopener stays shut). A desktop that holds no
//! device key sends nothing and says `crate::backend::registry::no_device_key`.
//!
//! # ⚠ Refused by name, client-side, against a server that cannot answer
//!
//! [`vike_tradehub_client::exchange_once`] is handed BOTH capability strings the verbs owe
//! (`account-verbs` and `credential-admin`); a node that advertises fewer is sent NOTHING after the
//! handshake, and the screen says the server does not offer credential administration. No value
//! reaches a log line, an error text or a `Debug` of this module.

use std::sync::{Arc, Mutex};

use vike_hub_protocol::access::DeviceKey;
use vike_hub_protocol::auth::Scope;
use vike_hub_protocol::call::HubCallError;
use vike_model::secret::Secret;
use vike_tradehub_client::proto::{
    FEATURE_ACCOUNT_VERBS, FEATURE_CREDENTIAL_ADMIN, Request, Response,
};
use vike_tradehub_client::wire::{AccountRequestWire, AccountVerbWire};
use vike_utility::mutex::lock;
use vike_venues::credential_keys::{FINNHUB_API_KEY, FMP_API_KEY};

/// One data-provider key as the screen shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BackendProviderKey {
    /// The credential-store name — `vike_venues::credential_keys`' own constant.
    pub(crate) name: &'static str,
    /// The provider, as the row's title.
    pub(crate) label: &'static str,
    /// What the key serves, as the row's second line.
    pub(crate) serves: &'static str,
}

/// The two keys, in screen order. The names are `vike_venues::credential_keys::DATA_PROVIDER_KEYS`
/// (`the_screen_rows_are_the_closed_pair_the_wire_admits` holds the two equal).
pub(crate) const PROVIDER_KEYS: [BackendProviderKey; 2] = [
    BackendProviderKey { name: FINNHUB_API_KEY, label: "Finnhub key", serves: "earnings calendar" },
    BackendProviderKey { name: FMP_API_KEY, label: "FMP key", serves: "dividends calendar" },
];

/// Both capabilities every verb here owes (see the module doc).
const NEEDS: [&str; 2] = [FEATURE_ACCOUNT_VERBS, FEATURE_CREDENTIAL_ADMIN];

/// Send one account request to `address` under the admin scope, signed with `device` (this desktop's
/// device key, decision 0288), and return the node's reply, with every failure worded for the
/// operator: a refusal the node ANSWERED is its own text; a transport, handshake or negotiation
/// failure is worded here. ⚠ Nothing here formats the request (its `Debug` redacts a value, but a
/// message built from it would not be this module's to vouch for).
fn ask(
    address: &str,
    device: &DeviceKey,
    verb: AccountVerbWire,
    confirm: Option<String>,
) -> Result<Response, String> {
    let request = Request::Account(Box::new(AccountRequestWire { verb, confirm }));
    match vike_tradehub_client::exchange_once(address, device, Scope::Account, &NEEDS, &request) {
        Ok(Response::Error(refusal)) => Err(refusal),
        Ok(Response::Refused(refusal)) => Err(refusal.text),
        Ok(reply) => Ok(reply),
        Err(e @ HubCallError::AccessDenied(_)) => Err(format!(
            "the server refused this desktop ({e}). Credential administration needs this PC's \
             device to hold the role admin on the server's access list, and the server must have \
             DECLARED its account-administration barrier (an operator act on that box)."
        )),
        Err(e @ HubCallError::NotOffered { .. }) => Err(format!(
            "this server does not offer credential administration — it predates it, or its \
             account verbs are not armed. {e}"
        )),
        Err(e) => Err(format!("could not reach the server: {e}")),
    }
}

/// Which of `PROVIDER_KEYS` the node's store holds — the NAMES, never a value.
///
/// # Errors
/// The operator-worded reason the node could not be asked or refused.
pub fn check(address: &str, device: &DeviceKey) -> Result<Vec<String>, String> {
    let names = PROVIDER_KEYS.iter().map(|k| k.name.to_string()).collect();
    match ask(address, device, AccountVerbWire::HeldCredentials { names }, None)? {
        Response::CredentialsHeld(held) => Ok(held.held),
        _ => Err(UNEXPECTED_REPLY.to_string()),
    }
}

/// Write `value` as the credential `key` on the node. The wire's `confirm` is the key's own name,
/// filled here: pressing Save with a typed value is the deliberate act.
///
/// # Errors
/// The operator-worded reason the node could not be asked or refused. ⚠ Never quotes `value`: a node's refusal is its
/// own text, so the value this client sent is masked out of it should the node quote it.
pub fn save(address: &str, device: &DeviceKey, key: &str, value: &str) -> Result<String, String> {
    let verb = AccountVerbWire::SetCredential { key: key.to_string(), value: value.to_string() };
    let sent = Secret::new(value.to_string());
    written(ask(address, device, verb, Some(key.to_string())).map_err(|why| sent.redact(&why))?)
}

/// Retire the credential `key` on the node: kept at rest, answered as not set from then on.
///
/// # Errors
/// The operator-worded reason the node could not be asked or refused.
pub fn remove(address: &str, device: &DeviceKey, key: &str) -> Result<String, String> {
    let verb = AccountVerbWire::RetireCredential { key: key.to_string() };
    written(ask(address, device, verb, Some(key.to_string()))?)
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
pub(crate) enum BackendProviderKeyMark {
    /// The node's store holds it.
    Set,
    /// The node answered and does not hold it.
    NotSet,
    /// Not asked yet, or the last ask failed.
    Unknown,
}

/// The result of the one job a [`BackendProviderKeysView`] may have in flight.
enum BackendProviderKeysSlot {
    Idle,
    Running,
    Done(BackendProviderKeysDone),
}

/// A finished job, by which key (an index into [`PROVIDER_KEYS`]) it was about.
enum BackendProviderKeysDone {
    Checked(Result<Vec<String>, String>),
    Saved(usize, Result<String, String>),
    Removed(usize, Result<String, String>),
}

/// The "Data providers" group's per-window state: one value buffer per key, the two-click Remove's
/// arm, what the node last said it holds, and the one background job in flight.
///
/// ⚠ **`Debug` is HAND-WRITTEN and prints no field that can hold a secret** — the value buffers are
/// where one can sit, and a derived `Debug` would put them in every `{:?}` and every test panic.
#[derive(Clone)]
pub struct BackendProviderKeysView {
    /// One value buffer per [`PROVIDER_KEYS`] row. Taken (emptied) the moment a Save is sent, so a
    /// value does not linger in UI memory after it has gone.
    pub(crate) values: [String; 2],
    /// The armed Remove: `(row index, egui time of the arming click)`. See [`Self::tick_arm`].
    pub(crate) armed: Option<(usize, f64)>,
    /// The names the node last said it holds; `None` until a check has answered.
    held: Option<Vec<String>>,
    /// The last act's outcome for the status line: `(is_error, text)`.
    status: Option<(bool, String)>,
    /// The backend address `held` was read from; a switch to another backend forgets the marks.
    address_seen: String,
    job: Arc<Mutex<BackendProviderKeysSlot>>,
}

impl Default for BackendProviderKeysView {
    fn default() -> Self {
        Self {
            values: [String::new(), String::new()],
            armed: None,
            held: None,
            status: None,
            address_seen: String::new(),
            job: Arc::new(Mutex::new(BackendProviderKeysSlot::Idle)),
        }
    }
}

impl std::fmt::Debug for BackendProviderKeysView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackendProviderKeysView")
            .field("armed", &self.armed.as_ref().map(|(i, _)| i))
            .field("held", &self.held)
            .field("busy", &self.busy())
            .finish_non_exhaustive()
    }
}

impl BackendProviderKeysView {
    /// How long an armed Remove waits for its second click.
    pub(crate) const ARM_WINDOW: std::time::Duration = std::time::Duration::from_secs(5);

    /// Is a job in flight? The screen disables its buttons meanwhile (one job at a time).
    pub(crate) fn busy(&self) -> bool {
        matches!(*self.slot(), BackendProviderKeysSlot::Running)
    }

    /// What the node last said about row `index`.
    pub(crate) fn mark(&self, index: usize) -> BackendProviderKeyMark {
        match (&self.held, PROVIDER_KEYS.get(index)) {
            (Some(held), Some(key)) if held.iter().any(|h| h == key.name) => {
                BackendProviderKeyMark::Set
            }
            (Some(_), Some(_)) => BackendProviderKeyMark::NotSet,
            _ => BackendProviderKeyMark::Unknown,
        }
    }

    /// The last act's outcome: `(is_error, text)`.
    pub(crate) fn status(&self) -> Option<&(bool, String)> {
        self.status.as_ref()
    }

    /// Expire an armed Remove whose [`Self::ARM_WINDOW`] ran out at egui time `now`. Returns the
    /// seconds still left when it is still armed (so the caller can repaint at the deadline).
    pub(crate) fn tick_arm(&mut self, now: f64) -> Option<f64> {
        let (_, armed_at) = self.armed?;
        let left = Self::ARM_WINDOW.as_secs_f64() - (now - armed_at);
        if left <= 0.0 {
            self.armed = None;
            None
        } else {
            Some(left)
        }
    }

    fn slot(&self) -> std::sync::MutexGuard<'_, BackendProviderKeysSlot> {
        lock(&self.job)
    }

    /// Fold a finished job into the view. Called once a frame, BEFORE drawing, with the backend
    /// address the frame is about: a different address forgets what another box said.
    pub(crate) fn poll(&mut self, address: Option<&str>, context: &egui::Context) {
        let address = address.unwrap_or_default();
        if address != self.address_seen {
            self.address_seen = address.to_string();
            self.held = None;
            self.status = None;
            self.armed = None;
        }
        let done = {
            let mut slot = self.slot();
            match std::mem::replace(&mut *slot, BackendProviderKeysSlot::Idle) {
                BackendProviderKeysSlot::Done(done) => Some(done),
                other => {
                    *slot = other;
                    None
                }
            }
        };
        if let Some(done) = done {
            self.apply(done, address, context);
        }
    }

    /// Words the outcome and, after a write that landed, asks the node what it holds now.
    fn apply(&mut self, done: BackendProviderKeysDone, address: &str, context: &egui::Context) {
        match done {
            BackendProviderKeysDone::Checked(Ok(held)) => self.held = Some(held),
            BackendProviderKeysDone::Checked(Err(e)) => {
                self.held = None;
                self.status = Some((true, e));
            }
            BackendProviderKeysDone::Saved(i, Ok(_)) => {
                self.status = Some((
                    false,
                    format!(
                        "{} saved on the server. The datahub re-reads it each cycle (about a \
                         minute); no restart is needed.",
                        PROVIDER_KEYS[i].name
                    ),
                ));
                self.refresh(address, context);
            }
            BackendProviderKeysDone::Removed(i, Ok(_)) => {
                self.status = Some((
                    false,
                    format!(
                        "Removed {} — kept at rest, Save a value to add it back",
                        PROVIDER_KEYS[i].name
                    ),
                ));
                self.refresh(address, context);
            }
            BackendProviderKeysDone::Saved(_, Err(e))
            | BackendProviderKeysDone::Removed(_, Err(e)) => self.status = Some((true, e)),
        }
    }

    /// Ask the node which keys it holds. A no-op while a job is in flight.
    pub(crate) fn start_check(&mut self, address: &str, context: &egui::Context) {
        if !self.busy() {
            self.status = None;
        }
        if self.device_or_say(address).is_some() {
            self.refresh(address, context);
        }
    }

    /// [`Self::start_check`] without touching the status line: the re-read after a write that
    /// landed, which must not wipe the sentence that write just earned (so no device key reads as
    /// nothing to re-read, never as a new status).
    fn refresh(&mut self, address: &str, context: &egui::Context) {
        let Some(device) = crate::backend::registry::device_key() else { return };
        let address = address.to_string();
        self.spawn(context, move || BackendProviderKeysDone::Checked(check(&address, &device)));
    }

    /// This desktop's device key, read afresh from the store (a connect, or `vike-cli backend
    /// mint-missing` in a terminal, may have just made it); without one, nothing is sent and the
    /// status line says why (decision 0288 §3).
    fn device_or_say(&mut self, address: &str) -> Option<DeviceKey> {
        let device = crate::backend::registry::fresh_device_key();
        if device.is_none() {
            self.status = Some((true, crate::backend::registry::no_device_key(address)));
        }
        device
    }

    /// Send row `index`'s typed value. The buffer is TAKEN: it is empty from this point on.
    pub(crate) fn start_save(&mut self, index: usize, address: &str, context: &egui::Context) {
        if self.busy() {
            return;
        }
        let value = std::mem::take(&mut self.values[index]);
        if value.trim().is_empty() {
            self.status = Some((true, "nothing entered — no changes saved".to_string()));
            return;
        }
        self.status = None;
        let Some(device) = self.device_or_say(address) else { return };
        let address = address.to_string();
        let name = PROVIDER_KEYS[index].name;
        self.spawn(context, move || {
            BackendProviderKeysDone::Saved(index, save(&address, &device, name, value.trim()))
        });
    }

    /// Send row `index`'s Remove (the second click).
    pub(crate) fn start_remove(&mut self, index: usize, address: &str, context: &egui::Context) {
        self.armed = None;
        self.status = None;
        let Some(device) = self.device_or_say(address) else { return };
        let address = address.to_string();
        let name = PROVIDER_KEYS[index].name;
        self.spawn(context, move || {
            BackendProviderKeysDone::Removed(index, remove(&address, &device, name))
        });
    }

    /// Run `work` on its own thread and leave its result in the slot, waking the UI. One job at a
    /// time: asking while one runs does nothing.
    fn spawn(
        &mut self,
        context: &egui::Context,
        work: impl FnOnce() -> BackendProviderKeysDone + Send + 'static,
    ) {
        {
            let mut slot = self.slot();
            if matches!(*slot, BackendProviderKeysSlot::Running) {
                return;
            }
            *slot = BackendProviderKeysSlot::Running;
        }
        let job = Arc::clone(&self.job);
        let context = context.clone();
        let spawned = std::thread::Builder::new().name("ui-provider-key".into()).spawn(move || {
            let done = work();
            *lock(&job) = BackendProviderKeysSlot::Done(done);
            context.request_repaint();
        });
        if let Err(e) = spawned {
            *self.slot() = BackendProviderKeysSlot::Idle;
            self.status = Some((true, format!("could not start the request: {e}")));
        }
    }

    /// TEST DOOR: leave a finished job in the slot, as the worker thread would.
    #[cfg(test)]
    fn finish(&self, done: BackendProviderKeysDone) {
        *self.slot() = BackendProviderKeysSlot::Done(done);
    }
}

#[path = "provider_keys_tests.rs"]
#[cfg(test)]
mod provider_keys_tests;
