//! Backends EDITOR — the add/edit/delete form state machine behind the Connections tool's
//! Backends section (split-plane I2): before this module the picker could list and connect, but
//! a record was born by hand-editing `backends.json`. Every DECISION lives here as a pure
//! function over caller-supplied state — what a click does ([`EditorState`]'s transitions,
//! [`submit`], [`confirm_delete`]), whether a form is admissible ([`validate`]), and the
//! binary-side drain order ([`apply_registry_update`]) — so it all runs in CI; egui only renders
//! (`crate::ui::tool_views::connections`).
//!
//! **The form holds credential-store KEY NAMES, never key material** — the same B7 contract
//! `crate::backend::backend_registry`'s module doc argues at length ([`KEY_NAME_HINT`] restates it in the
//! form itself). The one concession to typo-catching is [`key_presence`]: a read-only membership
//! check of the typed NAME against the caller-supplied credentials map (the same map the
//! Connections grid already receives) — it answers present/absent and can never surface a value.
//!
//! **Validation runs BEFORE the save**, deliberately ahead of `backend_registry::save`'s own
//! sanitize-on-save: the saver would silently RENAME a hostile name, which is the right
//! last-line defence for a hand-edited file but the wrong UX for a form — the operator must see
//! the refusal ([`FormError::NameNotClean`] names what the name would become), not discover a
//! silent rename later. A form that validates is therefore always a fixpoint of the saver's
//! sanitizer, so what the operator confirmed is byte-for-byte what lands on disk.
//!
//! **The two active-backend rules** (the live connection is [`super::backend_conn::BackendConn`]'s,
//! not this module's, so both are expressed as DATA for the binary to act on):
//!
//! - **Deleting the ACTIVE backend disconnects FIRST**, through the same switch machinery as a
//!   picker click — never a dangling conn on a record the registry no longer holds.
//!   [`confirm_delete`] flags it ([`RegistryUpdate::disconnect_first`]) and
//!   [`apply_registry_update`] pins the order: disconnect, then save.
//! - **Editing the ACTIVE backend takes effect on NEXT connect** — the record is saved, the live
//!   conn is deliberately NOT hot-rewired (a half-rewired observe bridge is worse than a stale
//!   one; the switch routine exists precisely so a reconnect is one click). [`edit_defers`] is
//!   the UI's cue to say so; a submitted edit never sets `disconnect_first`.

use std::collections::HashMap;

use crate::backend::backend_registry::{BackendRecord, BackendsFile, sanitize_backend_name};

/// The B7 restatement the form renders as its hint line — key NAMES, never key material.
pub const KEY_NAME_HINT: &str = "key fields hold credential-store KEY NAMES (e.g. \
PROD2_OBSERVE_KEY), never the key itself — values live in the credential store only";

/// What the operator has typed — raw, untrimmed, exactly as the text edits hold it.
/// [`validate`] normalizes (trims) on the way to a [`BackendRecord`]; a blank
/// [`BackendForm::control_key`] becomes `None` (no control key named).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackendForm {
    /// Display name — must be non-empty and sanitizer-clean to save (module doc).
    pub name: String,
    /// Dial address — must be `host:port`-shaped ([`addr_is_host_port`]).
    pub addr: String,
    /// Credential-store KEY NAME of the observe key — required (a keyless handshake is refused
    /// by the daemon, so an empty name can only ever build a backend that never connects).
    pub observe_key: String,
    /// Credential-store KEY NAME of the control key — optional; blank ⇒ `None`.
    pub control_key: String,
    /// The per-backend arming gate (B9) as the checkbox holds it.
    pub control: bool,
}

impl BackendForm {
    /// Seed the form from an existing record (the Edit path).
    pub fn from_record(rec: &BackendRecord) -> Self {
        BackendForm {
            name: rec.name.clone(),
            addr: rec.addr.clone(),
            observe_key: rec.observe_key.clone(),
            control_key: rec.control_key.clone().unwrap_or_default(),
            control: rec.control,
        }
    }
}

/// One admissibility failure, operator-readable through `Display`. [`validate`] collects ALL of
/// them rather than stopping at the first, so one Save click surfaces the whole repair list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormError {
    /// Name empty (or whitespace-only).
    NameEmpty,
    /// Name is not a fixpoint of the saver's sanitizer — saving would silently rename it.
    NameNotClean {
        /// What `backend_registry`'s sanitize-on-save WOULD turn the name into.
        sanitized: String,
    },
    /// Another record already carries this name — the refusal names the clash.
    DuplicateName {
        /// The existing record's name.
        clash: String,
    },
    /// Address is not `host:port`-shaped.
    AddrNotHostPort,
    /// No observe key NAME given.
    ObserveKeyEmpty,
}

impl std::fmt::Display for FormError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FormError::NameEmpty => write!(f, "name must not be empty"),
            FormError::NameNotClean { sanitized } => write!(
                f,
                "name would be silently renamed on save — use {sanitized:?} \
                 (letters, digits, spaces, - and _ only)"
            ),
            FormError::DuplicateName { clash } => {
                write!(f, "a backend named {clash:?} already exists")
            }
            FormError::AddrNotHostPort => {
                write!(f, "addr must be host:port (e.g. 127.0.0.1:9040)")
            }
            FormError::ObserveKeyEmpty => write!(
                f,
                "observe key name must not be empty — the daemon refuses a keyless handshake"
            ),
        }
    }
}

/// Is `addr` `host:port`-shaped? Split on the LAST `:` (so a bracketed — or even bare — IPv6
/// host keeps its colons), non-empty host, and a port that parses as a non-zero `u16`.
pub fn addr_is_host_port(addr: &str) -> bool {
    let Some((host, port)) = addr.rsplit_once(':') else {
        return false;
    };
    !host.is_empty()
        && !port.is_empty()
        && port.chars().all(|c| c.is_ascii_digit())
        && port.parse::<u16>().is_ok_and(|p| p != 0)
}

/// Validate a form against the current registry: `editing` is the ORIGINAL name of the record
/// being edited (`None` on the Add path), which is how the duplicate check excuses a record from
/// clashing with itself. `Ok` carries the normalized [`BackendRecord`] — trimmed fields, blank
/// `control_key` as `None` — which the sanitizer provably leaves unchanged (the
/// [`FormError::NameNotClean`] check IS the fixpoint proof).
pub fn validate(
    form: &BackendForm,
    file: &BackendsFile,
    editing: Option<&str>,
) -> Result<BackendRecord, Vec<FormError>> {
    let name = form.name.trim();
    let addr = form.addr.trim();
    let observe_key = form.observe_key.trim();
    let control_key = form.control_key.trim();

    let mut errors = Vec::new();
    if name.is_empty() {
        errors.push(FormError::NameEmpty);
    } else {
        let clean = sanitize_backend_name(name);
        if clean != name {
            errors.push(FormError::NameNotClean { sanitized: clean });
        } else if file.backends.iter().any(|r| r.name == name && editing != Some(r.name.as_str())) {
            errors.push(FormError::DuplicateName { clash: name.to_string() });
        }
    }
    if !addr_is_host_port(addr) {
        errors.push(FormError::AddrNotHostPort);
    }
    if observe_key.is_empty() {
        errors.push(FormError::ObserveKeyEmpty);
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(BackendRecord {
        name: name.to_string(),
        addr: addr.to_string(),
        observe_key: observe_key.to_string(),
        control_key: (!control_key.is_empty()).then(|| control_key.to_string()),
        control: form.control,
        datahub_observe_key: String::new(),
    })
}

/// Armed but keyless — `control = true` with no control key NAMED. Not a refusal (it is a valid
/// B9 state: the record simply can never mount a write channel) but worth a form notice, since
/// an operator ticking "armed" almost certainly expected it to arm something.
pub fn armed_without_key(form: &BackendForm) -> bool {
    form.control && form.control_key.trim().is_empty()
}

/// The typo-catcher's three answers ([`key_presence`]) — presence only, never a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPresence {
    /// No name typed yet — nothing to check.
    Blank,
    /// The named key EXISTS in the caller-supplied credentials map.
    Present,
    /// The named key is NOT in the map — likely a typo (or a key still to be added; either way
    /// the operator should look, which is all this indicator asks).
    Absent,
}

/// Read-only membership check of a typed key NAME against the caller-supplied credentials map —
/// the same map the Connections grid already receives, so this adds no read of anything. The
/// return type is the whole output: no value ever leaves the map through here.
pub fn key_presence(name: &str, vars: &HashMap<String, String>) -> KeyPresence {
    let name = name.trim();
    if name.is_empty() {
        KeyPresence::Blank
    } else if vars.contains_key(name) {
        KeyPresence::Present
    } else {
        KeyPresence::Absent
    }
}

/// The editor's whole state machine — one instance on the `App`, threaded into the Connections
/// tool body mutably each frame. Every transition is a method (or [`submit`] /
/// [`confirm_delete`], which need the registry too); egui reads the state and renders.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum EditorState {
    /// No form showing.
    #[default]
    Closed,
    /// The Add form: an empty [`BackendForm`] plus the previous Save click's refusals.
    Add {
        /// What the operator has typed so far.
        form: BackendForm,
        /// The last refused Save's errors (empty until a refusal).
        errors: Vec<FormError>,
    },
    /// The Edit form, remembering WHICH record it edits by its pre-edit name (the name itself is
    /// editable, so the form field cannot double as the identity).
    Edit {
        /// The record's name as it stood when Edit was clicked — the duplicate check's
        /// self-exclusion and [`apply_edit`]'s lookup key.
        original: String,
        /// The record's fields, editable.
        form: BackendForm,
        /// The last refused Save's errors.
        errors: Vec<FormError>,
    },
    /// The are-you-sure affordance: Delete was clicked on `name`, nothing has happened yet, and
    /// nothing will until [`confirm_delete`] (or [`EditorState::cancel`]).
    ConfirmDelete {
        /// The record awaiting confirmation.
        name: String,
    },
}

impl EditorState {
    /// Add clicked: open an empty form (dropping any other in-flight editor state).
    pub fn open_add(&mut self) {
        *self = EditorState::Add { form: BackendForm::default(), errors: Vec::new() };
    }

    /// Edit clicked on a row: open the form seeded from the record, remembering its identity.
    pub fn open_edit(&mut self, rec: &BackendRecord) {
        *self = EditorState::Edit {
            original: rec.name.clone(),
            form: BackendForm::from_record(rec),
            errors: Vec::new(),
        };
    }

    /// Delete clicked on a row: ask first. The registry is untouched until the confirm.
    pub fn request_delete(&mut self, name: &str) {
        *self = EditorState::ConfirmDelete { name: name.to_string() };
    }

    /// Cancel clicked (any state): close, discarding the form.
    pub fn cancel(&mut self) {
        *self = EditorState::Closed;
    }
}

/// One drained editor outcome for the binary to apply after the frame — the same OUT-slot idiom
/// as [`backend_conn::BackendAction`](crate::backend::backend_conn::BackendAction). Carries the WHOLE new
/// registry file (not a diff): the App assigns it and saves it, so the file on disk and the file
/// in memory can never disagree about what was just confirmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryUpdate {
    /// The registry as it should be after this update.
    pub file: BackendsFile,
    /// `true` ⇒ this update deletes the ACTIVE backend: the binary must disconnect (through the
    /// existing switch machinery) BEFORE saving — [`apply_registry_update`] pins the order.
    pub disconnect_first: bool,
}

/// Save clicked on the Add/Edit form: validate, and either close the editor and hand back the
/// updated registry (add appends; edit replaces in place, the `active` pointer following a
/// rename), or stash the refusals in the open form and hand back nothing. An edit never asks for
/// a disconnect — editing the ACTIVE backend deliberately defers to the next connect
/// ([`edit_defers`] is the UI's cue; module doc has the argument).
pub fn submit(state: &mut EditorState, file: &BackendsFile) -> Option<RegistryUpdate> {
    let (form, editing) = match state {
        EditorState::Add { form, .. } => (form.clone(), None),
        EditorState::Edit { original, form, .. } => (form.clone(), Some(original.clone())),
        _ => return None,
    };
    match validate(&form, file, editing.as_deref()) {
        Ok(mut rec) => {
            let new_file = match editing.as_deref() {
                None => apply_add(file, rec),
                Some(original) => {
                    // ⚠ CARRY FORWARD every record field this FORM does not carry. Today that is
                    // `datahub_observe_key` (the market-data plane's key NAME — see its doc for why
                    // it is deliberately not an editor box: it is a platform key whose default is
                    // right on every ordinary box, and a custom name only resolves alongside a
                    // platform one in the same file). Without this, an unrelated edit — renaming a
                    // backend, ticking `control` — would silently WIPE a hand-set name and the
                    // symptom would be `bad mac` at the datahub handshake with nothing changed that
                    // could explain it. A form that gains a box for a field takes it out of here.
                    if let Some(prev) = file.backends.iter().find(|r| r.name == original) {
                        rec.datahub_observe_key.clone_from(&prev.datahub_observe_key);
                    }
                    apply_edit(file, original, rec)
                }
            };
            *state = EditorState::Closed;
            Some(RegistryUpdate { file: new_file, disconnect_first: false })
        }
        Err(errs) => {
            if let EditorState::Add { errors, .. } | EditorState::Edit { errors, .. } = state {
                *errors = errs;
            }
            None
        }
    }
}

/// Confirm clicked on the are-you-sure: close the affordance and hand back the registry without
/// the record, flagged `disconnect_first` iff the deleted record is the LIVE connection's
/// ([`delete_disconnects`]).
pub fn confirm_delete(
    state: &mut EditorState,
    file: &BackendsFile,
    live_active: Option<&BackendRecord>,
) -> Option<RegistryUpdate> {
    let EditorState::ConfirmDelete { name } = state else {
        return None;
    };
    let name = name.clone();
    *state = EditorState::Closed;
    Some(RegistryUpdate {
        file: apply_delete(file, &name),
        disconnect_first: delete_disconnects(&name, live_active),
    })
}

/// Does deleting `name` require disconnecting first? Yes iff the live connection's record
/// carries that name. The empty name never matches: the `--observe ADDR` synthetic record is
/// unnamed and unlisted, so no registry row can legitimately claim it.
pub fn delete_disconnects(name: &str, live_active: Option<&BackendRecord>) -> bool {
    !name.is_empty() && live_active.is_some_and(|r| r.name == name)
}

/// Is this edit touching the ACTIVE backend's record — i.e. should the form show the
/// takes-effect-on-next-connect line? Same name-match rule as [`delete_disconnects`].
pub fn edit_defers(original: &str, live_active: Option<&BackendRecord>) -> bool {
    !original.is_empty() && live_active.is_some_and(|r| r.name == original)
}

/// Add: the registry plus the new record at the end (display order is file order).
pub fn apply_add(file: &BackendsFile, rec: BackendRecord) -> BackendsFile {
    let mut out = file.clone();
    out.backends.push(rec);
    out
}

/// Edit: replace the record named `original` in place (position preserved), the `active`
/// pointer retargeted through a rename so it keeps naming the same record. An `original` the
/// file no longer holds (edited away underneath the open form) edits nothing.
pub fn apply_edit(file: &BackendsFile, original: &str, rec: BackendRecord) -> BackendsFile {
    let mut out = file.clone();
    if let Some(slot) = out.backends.iter_mut().find(|r| r.name == original) {
        *slot = rec.clone();
        if out.active.as_deref() == Some(original) {
            out.active = Some(rec.name);
        }
    }
    out
}

/// Delete: the registry without every record named `name`, the `active` pointer cleared if it
/// pointed there (a pointer at a deleted record would resurrect it as "active but unlisted").
pub fn apply_delete(file: &BackendsFile, name: &str) -> BackendsFile {
    let mut out = file.clone();
    out.backends.retain(|r| r.name != name);
    if out.active.as_deref() == Some(name) {
        out.active = None;
    }
    out
}

/// The binary-side drain, order pinned: `disconnect` runs BEFORE `save` whenever the update
/// demands it, so the conn on a just-deleted record is torn down (through the same switch
/// machinery as a picker Disconnect — the caller passes exactly that) before the registry
/// forgets the record — never a dangling conn. Returns the file for the caller to assign as its
/// in-memory registry.
pub fn apply_registry_update<D, S>(upd: RegistryUpdate, disconnect: D, save: S) -> BackendsFile
where
    D: FnOnce(),
    S: FnOnce(&BackendsFile),
{
    if upd.disconnect_first {
        disconnect();
    }
    save(&upd.file);
    upd.file
}

#[path = "backend_editor_tests.rs"]
#[cfg(test)]
mod backend_editor_tests;
