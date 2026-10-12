use super::*;
use std::sync::{Arc, Mutex};

fn rec(name: &str) -> BackendRecord {
    BackendRecord {
        name: name.to_string(),
        addr: "the CI box.example:9040".to_string(),
        observe_key: "PROD2_OBSERVE_KEY".to_string(),
        control_key: Some("PROD2_CONTROL_KEY".to_string()),
        control: false,
        datahub_observe_key: String::new(),
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
    assert!(!edit_defers("", Some(&crate::backend::backend_conn::cli_observe_record("h:1"))));
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

/// ⚠ An edit CARRIES FORWARD the record fields the form does not carry — today
/// `datahub_observe_key`. Without this, ticking `control` on an unrelated row would silently
/// wipe the market-data plane's key NAME, and the only symptom would be `bad mac` at the
/// datahub handshake.
#[test]
fn an_edit_preserves_the_datahub_key_name_the_form_does_not_carry() {
    let mut file = file_with(&["the CI box"]);
    file.backends[0].datahub_observe_key = "PROD2_DATAHUB_KEY".to_string();
    let mut s = EditorState::default();
    s.open_edit(&file.backends[0]);
    if let EditorState::Edit { form, .. } = &mut s {
        form.control = true;
        form.control_key = "PROD2_CONTROL_KEY".to_string();
    }
    let upd = submit(&mut s, &file).expect("a valid edit submits");
    assert_eq!(
        upd.file.backends[0].datahub_observe_key, "PROD2_DATAHUB_KEY",
        "an unrelated edit wiped the datahub key NAME"
    );
    assert!(upd.file.backends[0].control, "...and the edit itself still applied");
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
    assert!(!delete_disconnects(
        "",
        Some(&crate::backend::backend_conn::cli_observe_record("h:1"))
    ));
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
    crate::backend::backend_registry::save_to(&file, &p).expect("save");
    let back = crate::backend::backend_registry::load_from(&p).expect("load");
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
