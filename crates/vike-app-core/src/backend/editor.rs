//! Backends EDITOR — the add/edit/delete form state machine behind the Connections tool's
//! Backends section (split-plane I2): before this module the picker could list and connect, but
//! a record was born by hand-editing `backends.json`. Every DECISION lives here as a pure
//! function over caller-supplied state — what a click does ([`BackendEditorState`]'s transitions,
//! [`submit`], `confirm_delete`), whether a form is admissible (`validate`), and the
//! binary-side drain order ([`apply_registry_update`]) — so it all runs in CI; egui only renders
//! (`crate::ui::windows::backend`).
//!
//! **The form holds no key and no key name** — every dial signs with this desktop's device key
//! (`crate::backend::registry`'s module doc, decision 0288), so a record is a name, an address and
//! its arming. A record it saves writes the retired [`BackendRecord::observe_key`] /
//! [`BackendRecord::control_key`] empty.
//!
//! **Validation runs BEFORE the save**, deliberately ahead of `registry::save`'s own
//! sanitize-on-save: the saver would silently RENAME a hostile name, which is the right
//! last-line defence for a hand-edited file but the wrong UX for a form — the operator must see
//! the refusal ([`BackendFormError::NameNotClean`] names what the name would become), not discover a
//! silent rename later. A form that validates is therefore always a fixpoint of the saver's
//! sanitizer, so what the operator confirmed is byte-for-byte what lands on disk.
//!
//! **The two active-backend rules** (the live connection is [`super::connection::BackendConnection`]'s,
//! not this module's, so both are expressed as DATA for the binary to act on):
//!
//! - **Deleting the ACTIVE backend disconnects FIRST**, through the same switch machinery as a
//!   picker click — never a dangling conn on a record the registry no longer holds.
//!   `confirm_delete` flags it (`BackendRegistryUpdate::disconnect_first`) and
//!   [`apply_registry_update`] pins the order: disconnect, then save.
//! - **Editing the ACTIVE backend takes effect on NEXT connect** — the record is saved, the live
//!   conn is deliberately NOT hot-rewired (a half-rewired observe bridge is worse than a stale
//!   one; the switch routine exists precisely so a reconnect is one click). `edit_defers` is
//!   the UI's cue to say so; a submitted edit never sets `disconnect_first`.

use crate::backend::registry::{BackendRecord, BackendRegistry};
use crate::ui::window::workspace::sanitize_layout_name;

/// What the operator has typed — raw, untrimmed, exactly as the text edits hold it.
/// `validate` normalizes (trims) on the way to a [`BackendRecord`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackendForm {
    /// Display name — must be non-empty and sanitizer-clean to save (module doc).
    pub name: String,
    /// Dial address — must be `host:port`-shaped (`address_is_host_port`).
    pub address: String,
    /// The per-backend arming gate (B9) as the checkbox holds it.
    pub control: bool,
}

impl BackendForm {
    /// Seed the form from an existing record (the Edit path).
    pub(crate) fn from_record(rec: &BackendRecord) -> Self {
        BackendForm { name: rec.name.clone(), address: rec.address.clone(), control: rec.control }
    }
}

/// One admissibility failure, operator-readable through `Display`. `validate` collects ALL of
/// them rather than stopping at the first, so one Save click surfaces the whole repair list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendFormError {
    /// Name empty (or whitespace-only).
    NameEmpty,
    /// Name is not a fixpoint of the saver's sanitizer — saving would silently rename it.
    NameNotClean {
        /// What `backend::registry`'s sanitize-on-save WOULD turn the name into.
        sanitized: String,
    },
    /// Another record already carries this name — the refusal names the clash.
    DuplicateName {
        /// The existing record's name.
        clash: String,
    },
    /// Address is not `host:port`-shaped.
    AddressNotHostPort,
    /// `backends.json` on disk is not read ([`BackendRegistry::refused`], the hint): nothing is saved
    /// over it until it is moved away.
    FileNotRead(String),
}

impl std::fmt::Display for BackendFormError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackendFormError::NameEmpty => write!(f, "name must not be empty"),
            BackendFormError::NameNotClean { sanitized } => write!(
                f,
                "name would be silently renamed on save — use {sanitized:?} \
                 (letters, digits, spaces, - and _ only)"
            ),
            BackendFormError::DuplicateName { clash } => {
                write!(f, "a backend named {clash:?} already exists")
            }
            BackendFormError::AddressNotHostPort => {
                write!(f, "address must be host:port (e.g. 127.0.0.1:9040)")
            }
            BackendFormError::FileNotRead(hint) => f.write_str(hint),
        }
    }
}

/// Is `address` `host:port`-shaped? Split on the LAST `:` (so a bracketed — or even bare — IPv6
/// host keeps its colons), non-empty host, and a port that parses as a non-zero `u16`.
pub(crate) fn address_is_host_port(address: &str) -> bool {
    let Some((host, port)) = address.rsplit_once(':') else {
        return false;
    };
    !host.is_empty()
        && !port.is_empty()
        && port.chars().all(|c| c.is_ascii_digit())
        && port.parse::<u16>().is_ok_and(|p| p != 0)
}

/// Validate a form against the current registry: `editing` is the ORIGINAL name of the record
/// being edited (`None` on the Add path), which is how the duplicate check excuses a record from
/// clashing with itself. `Ok` carries the normalized [`BackendRecord`] — trimmed fields, the
/// retired key names empty — which the sanitizer provably leaves unchanged (the
/// [`BackendFormError::NameNotClean`] check IS the fixpoint proof).
pub(crate) fn validate(
    form: &BackendForm,
    file: &BackendRegistry,
    editing: Option<&str>,
) -> Result<BackendRecord, Vec<BackendFormError>> {
    let name = form.name.trim();
    let address = form.address.trim();

    let mut errors: Vec<BackendFormError> =
        file.refused.iter().cloned().map(BackendFormError::FileNotRead).collect();
    if name.is_empty() {
        errors.push(BackendFormError::NameEmpty);
    } else {
        let clean = sanitize_layout_name(name);
        if clean != name {
            errors.push(BackendFormError::NameNotClean { sanitized: clean });
        } else if file.backends.iter().any(|r| r.name == name && editing != Some(r.name.as_str())) {
            errors.push(BackendFormError::DuplicateName { clash: name.to_string() });
        }
    }
    if !address_is_host_port(address) {
        errors.push(BackendFormError::AddressNotHostPort);
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(BackendRecord {
        name: name.to_string(),
        address: address.to_string(),
        observe_key: String::new(),
        control_key: None,
        control: form.control,
    })
}

/// The editor's whole state machine — one instance on the `App`, threaded into the Connections
/// tool body mutably each frame. Every transition is a method (or [`submit`] /
/// `confirm_delete`, which need the registry too); egui reads the state and renders.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum BackendEditorState {
    /// No form showing.
    #[default]
    Closed,
    /// The Add form: an empty [`BackendForm`] plus the previous Save click's refusals.
    Add {
        /// What the operator has typed so far.
        form: BackendForm,
        /// The last refused Save's errors (empty until a refusal).
        errors: Vec<BackendFormError>,
    },
    /// The Edit form, remembering WHICH record it edits by its pre-edit name (the name itself is
    /// editable, so the form field cannot double as the identity).
    Edit {
        /// The record's name as it stood when Edit was clicked — the duplicate check's
        /// self-exclusion and `apply_edit`'s lookup key.
        original: String,
        /// The record's fields, editable.
        form: BackendForm,
        /// The last refused Save's errors.
        errors: Vec<BackendFormError>,
    },
    /// The are-you-sure affordance: Delete was clicked on `name`, nothing has happened yet, and
    /// nothing will until `confirm_delete` (or [`BackendEditorState::cancel`]).
    ConfirmDelete {
        /// The record awaiting confirmation.
        name: String,
    },
}

impl BackendEditorState {
    /// Add clicked: open an empty form (dropping any other in-flight editor state).
    pub(crate) fn open_add(&mut self) {
        *self = BackendEditorState::Add { form: BackendForm::default(), errors: Vec::new() };
    }

    /// Edit clicked on a row: open the form seeded from the record, remembering its identity.
    pub(crate) fn open_edit(&mut self, rec: &BackendRecord) {
        *self = BackendEditorState::Edit {
            original: rec.name.clone(),
            form: BackendForm::from_record(rec),
            errors: Vec::new(),
        };
    }

    /// Delete clicked on a row: ask first. The registry is untouched until the confirm.
    pub(crate) fn request_delete(&mut self, name: &str) {
        *self = BackendEditorState::ConfirmDelete { name: name.to_string() };
    }

    /// Cancel clicked (any state): close, discarding the form.
    pub fn cancel(&mut self) {
        *self = BackendEditorState::Closed;
    }
}

/// One drained editor outcome for the binary to apply after the frame — the same OUT-slot idiom
/// as [`connection::BackendAction`](crate::backend::connection::BackendAction). Carries the WHOLE new
/// registry file (not a diff): the App assigns it and saves it, so the file on disk and the file
/// in memory can never disagree about what was just confirmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendRegistryUpdate {
    /// The registry as it should be after this update.
    pub file: BackendRegistry,
    /// `true` ⇒ this update deletes the ACTIVE backend: the binary must disconnect (through the
    /// existing switch machinery) BEFORE saving — [`apply_registry_update`] pins the order.
    pub(crate) disconnect_first: bool,
}

/// Save clicked on the Add/Edit form: validate, and either close the editor and hand back the
/// updated registry (add appends; edit replaces in place, the `active` pointer following a
/// rename), or stash the refusals in the open form and hand back nothing. An edit never asks for
/// a disconnect — editing the ACTIVE backend deliberately defers to the next connect
/// (`edit_defers` is the UI's cue; module doc has the argument).
pub fn submit(
    state: &mut BackendEditorState,
    file: &BackendRegistry,
) -> Option<BackendRegistryUpdate> {
    let (form, editing) = match state {
        BackendEditorState::Add { form, .. } => (form.clone(), None),
        BackendEditorState::Edit { original, form, .. } => (form.clone(), Some(original.clone())),
        _ => return None,
    };
    match validate(&form, file, editing.as_deref()) {
        Ok(rec) => {
            // The form carries every record field, so an edit replaces the record whole.
            let new_file = match editing.as_deref() {
                None => apply_add(file, rec),
                Some(original) => apply_edit(file, original, rec),
            };
            *state = BackendEditorState::Closed;
            Some(BackendRegistryUpdate { file: new_file, disconnect_first: false })
        }
        Err(errs) => {
            if let BackendEditorState::Add { errors, .. }
            | BackendEditorState::Edit { errors, .. } = state
            {
                *errors = errs;
            }
            None
        }
    }
}

/// Confirm clicked on the are-you-sure: close the affordance and hand back the registry without
/// the record, flagged `disconnect_first` iff the deleted record is the LIVE connection's
/// ([`delete_disconnects`]).
pub(crate) fn confirm_delete(
    state: &mut BackendEditorState,
    file: &BackendRegistry,
    live_active: Option<&BackendRecord>,
) -> Option<BackendRegistryUpdate> {
    let BackendEditorState::ConfirmDelete { name } = state else {
        return None;
    };
    let name = name.clone();
    *state = BackendEditorState::Closed;
    Some(BackendRegistryUpdate {
        file: apply_delete(file, &name),
        disconnect_first: delete_disconnects(&name, live_active),
    })
}

/// Does deleting `name` require disconnecting first? Yes iff the live connection's record
/// carries that name. The empty name never matches: the `--observe ADDR` synthetic record is
/// unnamed and unlisted, so no registry row can legitimately claim it.
pub(crate) fn delete_disconnects(name: &str, live_active: Option<&BackendRecord>) -> bool {
    !name.is_empty() && live_active.is_some_and(|r| r.name == name)
}

/// Is this edit touching the ACTIVE backend's record — i.e. should the form show the
/// takes-effect-on-next-connect line? Same name-match rule as [`delete_disconnects`].
pub(crate) fn edit_defers(original: &str, live_active: Option<&BackendRecord>) -> bool {
    !original.is_empty() && live_active.is_some_and(|r| r.name == original)
}

/// Add: the registry plus the new record at the end (display order is file order).
pub(crate) fn apply_add(file: &BackendRegistry, rec: BackendRecord) -> BackendRegistry {
    let mut out = file.clone();
    out.backends.push(rec);
    out
}

/// Edit: replace the record named `original` in place (position preserved), the `active`
/// pointer retargeted through a rename so it keeps naming the same record. An `original` the
/// file no longer holds (edited away underneath the open form) edits nothing.
pub(crate) fn apply_edit(
    file: &BackendRegistry,
    original: &str,
    rec: BackendRecord,
) -> BackendRegistry {
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
pub(crate) fn apply_delete(file: &BackendRegistry, name: &str) -> BackendRegistry {
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
pub fn apply_registry_update<D, S>(
    upd: BackendRegistryUpdate,
    disconnect: D,
    save: S,
) -> BackendRegistry
where
    D: FnOnce(),
    S: FnOnce(&BackendRegistry),
{
    if upd.disconnect_first {
        disconnect();
    }
    save(&upd.file);
    upd.file
}

#[path = "editor_tests.rs"]
#[cfg(test)]
mod editor_tests;
