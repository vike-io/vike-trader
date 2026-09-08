//! Backends EDITOR — the add/edit/delete form state machine behind the Connections tool's
//! Backends section (split-plane I2): before this module the picker could list and connect, but
//! a record was born by hand-editing `backends.json`. Every DECISION lives here as a pure
//! function over caller-supplied state — what a click does ([`EditorState`]'s transitions,
//! [`submit`], [`confirm_delete`]), whether a form is admissible ([`validate`]), and the
//! binary-side drain order ([`apply_registry_update`]) — so it all runs in CI; egui only renders
//! (`crate::tool_views::connections`).
//!
//! **The form holds credential-store KEY NAMES, never key material** — the same B7 contract
//! `crate::backend_registry`'s module doc argues at length ([`KEY_NAME_HINT`] restates it in the
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
//! **The two active-backend rules** (the live connection is [`backend_conn::BackendConn`]'s,
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

use crate::backend_registry::{BackendRecord, BackendsFile, sanitize_backend_name};

/// The B7 restatement the form renders as its hint line — key NAMES, never key material.
pub const KEY_NAME_HINT: &str = "key fields hold credential-store KEY NAMES (e.g. \
PROD2_OBSERVE_KEY), never the key itself — values live in settings/secrets.env only";

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
/// as [`backend_conn::BackendAction`](crate::backend_conn::BackendAction). Carries the WHOLE new
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
        Ok(rec) => {
            let new_file = match editing.as_deref() {
                None => apply_add(file, rec),
                Some(original) => apply_edit(file, original, rec),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn rec(name: &str) -> BackendRecord {
        BackendRecord {
            name: name.to_string(),
            addr: "the CI box.example:9040".to_string(),
            observe_key: "PROD2_OBSERVE_KEY".to_string(),
            control_key: Some("PROD2_CONTROL_KEY".to_string()),
            control: false,
        }
    }

    fn file_with(names: &[&str]) -> BackendsFile {
        BackendsFile { backends: names.iter().map(|n| rec(n)).collect(), active: None }
    }

    fn valid_form(name: &str) -> BackendForm {
        BackendForm {
            name: name.to_string(),
            addr: "127.0.0.1:9040".to_string(),
            observe_key: "PROD2_OBSERVE_KEY".to_string(),
            control_key: String::new(),
            control: false,
        }
    }

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    // ── the validation table ────────────────────────────────────────────────────────────────

    /// Empty and whitespace-only names are refused as EMPTY (trim happens before every check).
    #[test]
    fn an_empty_name_is_refused() {
        for name in ["", "   ", "\t"] {
            let errs = validate(&valid_form(name), &file_with(&[]), None).unwrap_err();
            assert_eq!(errs, vec![FormError::NameEmpty], "name {name:?}");
        }
    }

    /// A hostile (path-shaped) name is REFUSED, naming what the saver's sanitizer would have
    /// silently turned it into — the operator sees the refusal, never the silent rename.
    #[test]
    fn a_hostile_name_is_refused_naming_what_it_would_become() {
        let errs = validate(&valid_form("../../evil"), &file_with(&[]), None).unwrap_err();
        assert_eq!(errs, vec![FormError::NameNotClean { sanitized: "evil".to_string() }]);
        // The message carries the clean form so the fix is one paste away.
        assert!(errs[0].to_string().contains("evil"), "{}", errs[0]);
    }

    /// A duplicate name is refused NAMING the clash — and the Edit path excuses the record from
    /// clashing with itself while still refusing a rename ONTO a sibling.
    #[test]
    fn a_duplicate_name_is_refused_naming_the_clash() {
        let file = file_with(&["the CI box", "prod3"]);
        let errs = validate(&valid_form("the CI box"), &file, None).unwrap_err();
        assert_eq!(errs, vec![FormError::DuplicateName { clash: "the CI box".to_string() }]);
        assert!(errs[0].to_string().contains("the CI box"), "the refusal must name the clash");

        // Editing the CI box while keeping its name: no clash.
        assert!(validate(&valid_form("the CI box"), &file, Some("the CI box")).is_ok());
        // Editing the CI box and renaming it onto prod3: clash.
        let errs = validate(&valid_form("prod3"), &file, Some("the CI box")).unwrap_err();
        assert_eq!(errs, vec![FormError::DuplicateName { clash: "prod3".to_string() }]);
    }

    /// The `host:port` gate, both sides: every malformed shape refused, the legitimate shapes
    /// (hostname, IPv4, bracketed IPv6) accepted.
    #[test]
    fn a_bad_addr_is_refused_and_a_good_one_accepted() {
        for bad in
            ["", "the CI box", ":9040", "the CI box:", "the CI box:abc", "the CI box:0", "the CI box:99999", "the CI box:90 40"]
        {
            let mut f = valid_form("a");
            f.addr = bad.to_string();
            let errs = validate(&f, &file_with(&[]), None).unwrap_err();
            assert_eq!(errs, vec![FormError::AddrNotHostPort], "addr {bad:?}");
        }
        for good in ["127.0.0.1:9040", "the CI box.example:9040", "[::1]:9040", "localhost:1"] {
            let mut f = valid_form("a");
            f.addr = good.to_string();
            assert!(validate(&f, &file_with(&[]), None).is_ok(), "addr {good:?} must pass");
        }
    }

    /// An empty observe-key NAME is refused: it can only ever build a backend whose every
    /// handshake the daemon refuses.
    #[test]
    fn an_empty_observe_key_is_refused() {
        let mut f = valid_form("a");
        f.observe_key = "  ".to_string();
        let errs = validate(&f, &file_with(&[]), None).unwrap_err();
        assert_eq!(errs, vec![FormError::ObserveKeyEmpty]);
    }

    /// One Save click surfaces the WHOLE repair list — errors accumulate, first does not win.
    #[test]
    fn errors_accumulate_rather_than_first_wins() {
        let form = BackendForm::default(); // everything blank
        let errs = validate(&form, &file_with(&[]), None).unwrap_err();
        assert_eq!(
            errs,
            vec![FormError::NameEmpty, FormError::AddrNotHostPort, FormError::ObserveKeyEmpty]
        );
    }

    /// A valid form normalizes into its record: fields trimmed, a blank control key as `None`,
    /// a named one kept — and the name is a FIXPOINT of the saver's sanitizer, so what was
    /// validated is byte-for-byte what `backend_registry::save` will write.
    #[test]
    fn a_valid_form_normalizes_into_its_record() {
        let form = BackendForm {
            name: "  the CI box live  ".to_string(),
            addr: " 127.0.0.1:9040 ".to_string(),
            observe_key: " OBS_KEY ".to_string(),
            control_key: "  ".to_string(),
            control: true,
        };
        let rec = validate(&form, &file_with(&[]), None).expect("valid");
        assert_eq!(rec.name, "the CI box live");
        assert_eq!(rec.addr, "127.0.0.1:9040");
        assert_eq!(rec.observe_key, "OBS_KEY");
        assert_eq!(rec.control_key, None, "blank control key must be None, not Some(\"\")");
        assert!(rec.control);
        assert_eq!(sanitize_backend_name(&rec.name), rec.name, "validated ⇒ sanitizer fixpoint");

        let mut named = form.clone();
        named.control_key = " CTL_KEY ".to_string();
        let rec = validate(&named, &file_with(&[]), None).expect("valid");
        assert_eq!(rec.control_key.as_deref(), Some("CTL_KEY"));
    }

    /// Armed-but-keyless is a NOTICE, not a refusal: the form flags it, validation passes.
    #[test]
    fn arming_without_a_control_key_is_flagged_not_refused() {
        let mut f = valid_form("a");
        f.control = true;
        f.control_key = String::new();
        assert!(armed_without_key(&f));
        assert!(validate(&f, &file_with(&[]), None).is_ok());
        f.control_key = "CTL".to_string();
        assert!(!armed_without_key(&f));
    }

    /// The typo-catcher's three states — blank field, name in the map, name not in the map —
    /// against the same shape of map the picker resolves keys from. Presence only: the return
    /// type has no value-carrying variant, so a key VALUE cannot leave the map through it.
    #[test]
    fn key_presence_indicator_states() {
        let m = vars(&[("PROD2_OBSERVE_KEY", "secret-bytes")]);
        assert_eq!(key_presence("", &m), KeyPresence::Blank);
        assert_eq!(key_presence("   ", &m), KeyPresence::Blank);
        assert_eq!(key_presence("PROD2_OBSERVE_KEY", &m), KeyPresence::Present);
        assert_eq!(key_presence(" PROD2_OBSERVE_KEY ", &m), KeyPresence::Present);
        assert_eq!(key_presence("PROD2_OBSRVE_KEY", &m), KeyPresence::Absent, "the typo case");
    }

    // ── the state machine ───────────────────────────────────────────────────────────────────

    /// Edit seeds the form from the record and remembers the pre-edit name as the identity.
    #[test]
    fn open_edit_seeds_the_form_from_the_record() {
        let mut s = EditorState::default();
        assert_eq!(s, EditorState::Closed);
        let mut r = rec("the CI box");
        r.control = true;
        s.open_edit(&r);
        let EditorState::Edit { original, form, errors } = &s else {
            panic!("expected Edit, got {s:?}");
        };
        assert_eq!(original, "the CI box");
        assert_eq!(form, &BackendForm::from_record(&r));
        assert_eq!(form.control_key, "PROD2_CONTROL_KEY", "Some(name) seeds the text field");
        assert!(errors.is_empty());
        s.cancel();
        assert_eq!(s, EditorState::Closed);
    }

    /// A refused Save keeps the form OPEN with the errors stashed for rendering — the operator's
    /// typing is not thrown away, and no update leaves the editor.
    #[test]
    fn a_refused_submit_keeps_the_form_open_with_errors() {
        let mut s = EditorState::default();
        s.open_add();
        // Type a bad addr into the open form.
        if let EditorState::Add { form, .. } = &mut s {
            *form = valid_form("the CI box");
            form.addr = "not-an-addr".to_string();
        }
        let upd = submit(&mut s, &file_with(&[]));
        assert_eq!(upd, None, "a refused submit must produce no update");
        let EditorState::Add { form, errors } = &s else {
            panic!("form must stay open, got {s:?}");
        };
        assert_eq!(form.name, "the CI box", "the typing survives the refusal");
        assert_eq!(errors, &vec![FormError::AddrNotHostPort]);
    }

    /// A valid Add closes the editor and hands back the file with the record appended — and
    /// never asks for a disconnect.
    #[test]
    fn a_valid_add_appends_and_closes() {
        let mut s = EditorState::default();
        s.open_add();
        if let EditorState::Add { form, .. } = &mut s {
            *form = valid_form("prod3");
        }
        let file = file_with(&["the CI box"]);
        let upd = submit(&mut s, &file).expect("valid add");
        assert_eq!(s, EditorState::Closed);
        assert!(!upd.disconnect_first);
        assert_eq!(
            upd.file.backends.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["the CI box", "prod3"],
            "append at the end, existing order untouched"
        );
    }

    /// EDIT-ACTIVE DEFERS: the record is saved (the update carries the edited file) and the live
    /// conn is untouched — `disconnect_first` is false and the update carries nothing else the
    /// binary could rewire with; `edit_defers` is what tells the form to say so.
    #[test]
    fn edit_active_defers_record_saved_conn_untouched() {
        let live = rec("the CI box"); // the live connection's record
        let mut s = EditorState::default();
        s.open_edit(&rec("the CI box"));
        if let EditorState::Edit { form, .. } = &mut s {
            form.addr = "the CI box.example:9999".to_string();
        }
        let file = file_with(&["the CI box", "prod3"]);
        let upd = submit(&mut s, &file).expect("valid edit");

        // The record IS saved…
        assert_eq!(upd.file.backends[0].addr, "the CI box.example:9999");
        // …and the conn is NOT touched: no disconnect, nothing to hot-rewire with.
        assert!(!upd.disconnect_first, "an edit must never disconnect — next connect picks it up");
        // The UI's deferred-effect line shows exactly for the ACTIVE record's edit.
        assert!(edit_defers("the CI box", Some(&live)));
        assert!(!edit_defers("prod3", Some(&live)));
        assert!(!edit_defers("the CI box", None));
        assert!(!edit_defers("", Some(&crate::backend_conn::cli_observe_record("h:1"))));
    }

    /// A rename retargets the `active` pointer with the record, so the pointer keeps naming the
    /// same record instead of dangling at the old name.
    #[test]
    fn an_edit_rename_retargets_the_active_pointer() {
        let mut file = file_with(&["the CI box", "prod3"]);
        file.active = Some("the CI box".to_string());
        let mut renamed = rec("the build runner");
        renamed.addr = "h:1".to_string();
        let out = apply_edit(&file, "the CI box", renamed);
        assert_eq!(out.active.as_deref(), Some("the build runner"));
        assert_eq!(out.backends[0].name, "the build runner", "position preserved");
        assert_eq!(out.backends[1].name, "prod3");

        // A pointer at a DIFFERENT record does not move.
        file.active = Some("prod3".to_string());
        let out = apply_edit(&file, "the CI box", rec("the build runner"));
        assert_eq!(out.active.as_deref(), Some("prod3"));
    }

    /// DELETE NEEDS THE CONFIRM: Delete on a row only opens the are-you-sure — no update leaves
    /// the editor, and Cancel walks it back with the registry untouched.
    #[test]
    fn delete_requires_confirmation_and_cancel_walks_back() {
        let mut s = EditorState::default();
        s.request_delete("the CI box");
        assert_eq!(s, EditorState::ConfirmDelete { name: "the CI box".to_string() });
        s.cancel();
        assert_eq!(s, EditorState::Closed);
        // confirm_delete on a non-confirming state is a no-op.
        assert_eq!(confirm_delete(&mut s, &file_with(&["the CI box"]), None), None);
    }

    /// DELETE-ACTIVE DISCONNECTS FIRST — proven through a recording double of the binary's two
    /// effects: the drain runs the disconnect (the existing switch machinery) BEFORE the save,
    /// and the returned file no longer holds the record nor an `active` pointer at it.
    #[test]
    fn delete_active_disconnects_first_through_the_drain() {
        let live = rec("the CI box");
        let mut file = file_with(&["the CI box", "prod3"]);
        file.active = Some("the CI box".to_string());

        let mut s = EditorState::default();
        s.request_delete("the CI box");
        let upd = confirm_delete(&mut s, &file, Some(&live)).expect("confirmed");
        assert_eq!(s, EditorState::Closed);
        assert!(upd.disconnect_first, "deleting the ACTIVE backend must disconnect first");

        // The recording double: both effects logged in drain order.
        let log: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let (l1, l2) = (Arc::clone(&log), Arc::clone(&log));
        let new_file = apply_registry_update(
            upd,
            move || l1.lock().unwrap().push("disconnect"),
            move |_| l2.lock().unwrap().push("save"),
        );
        assert_eq!(*log.lock().unwrap(), vec!["disconnect", "save"], "disconnect BEFORE save");
        assert_eq!(
            new_file.backends.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["prod3"]
        );
        assert_eq!(new_file.active, None, "the pointer at the deleted record is cleared");
    }

    /// Deleting a NON-active record never disconnects: the drain runs the save alone.
    #[test]
    fn delete_inactive_never_disconnects() {
        let live = rec("the CI box");
        let file = file_with(&["the CI box", "prod3"]);
        let mut s = EditorState::default();
        s.request_delete("prod3");
        let upd = confirm_delete(&mut s, &file, Some(&live)).expect("confirmed");
        assert!(!upd.disconnect_first);

        let log: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let (l1, l2) = (Arc::clone(&log), Arc::clone(&log));
        apply_registry_update(
            upd,
            move || l1.lock().unwrap().push("disconnect"),
            move |_| l2.lock().unwrap().push("save"),
        );
        assert_eq!(*log.lock().unwrap(), vec!["save"], "no disconnect for an inactive delete");

        // And with NO live connection at all, deleting anything never disconnects.
        assert!(!delete_disconnects("the CI box", None));
        // The unnamed synthetic `--observe` record can never be matched by a row delete.
        assert!(!delete_disconnects("", Some(&crate::backend_conn::cli_observe_record("h:1"))));
    }

    // ── the real save/load round-trip ───────────────────────────────────────────────────────

    /// The editor's output round-trips through the REAL registry save/load: an add + an edit +
    /// a delete driven through the state machine, written by `backend_registry`'s own saver
    /// (sanitize + atomic write) and read back equal — and the file on disk holds key NAMES
    /// only, never the value the credentials map carried for one of them.
    #[test]
    fn editor_output_round_trips_through_the_real_save_load() {
        // The credentials map, as the form's typo-catcher sees it — the ONE place the editor
        // ever meets key material's container, and it answers presence only.
        let m = vars(&[("PROD2_OBSERVE_KEY", "secret-bytes")]);
        assert_eq!(key_presence("PROD2_OBSERVE_KEY", &m), KeyPresence::Present);

        // Add "the CI box".
        let mut s = EditorState::default();
        s.open_add();
        if let EditorState::Add { form, .. } = &mut s {
            *form = valid_form("the CI box");
        }
        let file = submit(&mut s, &BackendsFile::default()).expect("add").file;

        // Add "prod3", then edit it: rename + arm control.
        s.open_add();
        if let EditorState::Add { form, .. } = &mut s {
            *form = valid_form("prod3");
        }
        let file = submit(&mut s, &file).expect("add 2").file;
        s.open_edit(&file.backends[1].clone());
        if let EditorState::Edit { form, .. } = &mut s {
            form.name = "prod3-live".to_string();
            form.control_key = "PROD3_CONTROL_KEY".to_string();
            form.control = true;
        }
        let file = submit(&mut s, &file).expect("edit").file;

        // Delete "the CI box".
        s.request_delete("the CI box");
        let file = confirm_delete(&mut s, &file, None).expect("delete").file;

        // Through the real saver/loader.
        let tmp = tempfile::tempdir().expect("temp dir");
        let p = tmp.path().join("backends.json");
        crate::backend_registry::save_to(&file, &p).expect("save");
        let back = crate::backend_registry::load_from(&p).expect("load");
        assert_eq!(back, file, "editor output must survive the disk round-trip unchanged");
        assert_eq!(back.backends.len(), 1);
        assert_eq!(back.backends[0].name, "prod3-live");
        assert!(back.backends[0].control);
        assert_eq!(back.backends[0].control_key.as_deref(), Some("PROD3_CONTROL_KEY"));

        // B7 on disk: names only. The map's VALUE for a named key never lands in the file.
        let raw = std::fs::read_to_string(&p).expect("raw json");
        assert!(raw.contains("PROD3_CONTROL_KEY"), "the NAME is the auditable surface");
        assert!(!raw.contains("secret-bytes"), "no key material may ever land in backends.json");
    }
}
