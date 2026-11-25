use super::*;
use crate::backend::registry::registry_tests::rec;
use std::sync::{Arc, Mutex};

fn file_with(names: &[&str]) -> BackendRegistry {
    BackendRegistry { backends: names.iter().map(|n| rec(n)).collect(), ..Default::default() }
}

fn valid_form(name: &str) -> BackendForm {
    BackendForm { name: name.to_string(), address: "127.0.0.1:9040".to_string(), control: false }
}

// ── the validation table ────────────────────────────────────────────────────────────────

/// Empty and whitespace-only names are refused as EMPTY (trim happens before every check).
#[test]
fn an_empty_name_is_refused() {
    for name in ["", "   ", "\t"] {
        let errs = validate(&valid_form(name), &file_with(&[]), None).unwrap_err();
        assert_eq!(errs, vec![BackendFormError::NameEmpty], "name {name:?}");
    }
}

/// A hostile (path-shaped) name is REFUSED, naming what the saver's sanitizer would have
/// silently turned it into — the operator sees the refusal, never the silent rename.
#[test]
fn a_hostile_name_is_refused_naming_what_it_would_become() {
    let errs = validate(&valid_form("../../evil"), &file_with(&[]), None).unwrap_err();
    assert_eq!(errs, vec![BackendFormError::NameNotClean { sanitized: "evil".to_string() }]);
    // The message carries the clean form so the fix is one paste away.
    assert!(errs[0].to_string().contains("evil"), "{}", errs[0]);
}

/// A duplicate name is refused NAMING the clash — and the Edit path excuses the record from
/// clashing with itself while still refusing a rename ONTO a sibling.
#[test]
fn a_duplicate_name_is_refused_naming_the_clash() {
    let file = file_with(&["the CI box", "prod3"]);
    let errs = validate(&valid_form("the CI box"), &file, None).unwrap_err();
    assert_eq!(errs, vec![BackendFormError::DuplicateName { clash: "the CI box".to_string() }]);
    assert!(errs[0].to_string().contains("the CI box"), "the refusal must name the clash");

    // Editing the CI box while keeping its name: no clash.
    assert!(validate(&valid_form("the CI box"), &file, Some("the CI box")).is_ok());
    // Editing the CI box and renaming it onto prod3: clash.
    let errs = validate(&valid_form("prod3"), &file, Some("the CI box")).unwrap_err();
    assert_eq!(errs, vec![BackendFormError::DuplicateName { clash: "prod3".to_string() }]);
}

/// The `host:port` gate, both sides: every malformed shape refused, the legitimate shapes
/// (hostname, IPv4, bracketed IPv6) accepted.
#[test]
fn a_bad_address_is_refused_and_a_good_one_accepted() {
    for bad in
        ["", "the CI box", ":9040", "the CI box:", "the CI box:abc", "the CI box:0", "the CI box:99999", "the CI box:90 40"]
    {
        let mut f = valid_form("a");
        f.address = bad.to_string();
        let errs = validate(&f, &file_with(&[]), None).unwrap_err();
        assert_eq!(errs, vec![BackendFormError::AddressNotHostPort], "address {bad:?}");
    }
    for good in ["127.0.0.1:9040", "the CI box.example:9040", "[::1]:9040", "localhost:1"] {
        let mut f = valid_form("a");
        f.address = good.to_string();
        assert!(validate(&f, &file_with(&[]), None).is_ok(), "address {good:?} must pass");
    }
}

/// One Save click surfaces the WHOLE repair list — errors accumulate, first does not win.
#[test]
fn errors_accumulate_rather_than_first_wins() {
    let form = BackendForm::default(); // everything blank
    let errs = validate(&form, &file_with(&[]), None).unwrap_err();
    assert_eq!(errs, vec![BackendFormError::NameEmpty, BackendFormError::AddressNotHostPort]);
}

/// A valid form normalizes into its record: fields trimmed, the retired key names empty — and the
/// name is a FIXPOINT of the saver's sanitizer, so what was validated is byte-for-byte what
/// `registry::save` will write.
#[test]
fn a_valid_form_normalizes_into_its_record() {
    let form = BackendForm {
        name: "  the CI box live  ".to_string(),
        address: " 127.0.0.1:9040 ".to_string(),
        control: true,
    };
    let rec = validate(&form, &file_with(&[]), None).expect("valid");
    assert_eq!(rec.name, "the CI box live");
    assert_eq!(rec.address, "127.0.0.1:9040");
    assert_eq!((rec.observe_key.as_str(), rec.control_key.as_deref()), ("", None), "no key");
    assert!(rec.control);
    assert_eq!(sanitize_layout_name(&rec.name), rec.name, "validated ⇒ sanitizer fixpoint");
}

// ── the state machine ───────────────────────────────────────────────────────────────────

/// Edit seeds the form from the record and remembers the pre-edit name as the identity.
#[test]
fn open_edit_seeds_the_form_from_the_record() {
    let mut s = BackendEditorState::default();
    assert_eq!(s, BackendEditorState::Closed);
    let mut r = rec("the CI box");
    r.control = true;
    s.open_edit(&r);
    let BackendEditorState::Edit { original, form, errors } = &s else {
        panic!("expected Edit, got {s:?}");
    };
    assert_eq!(original, "the CI box");
    assert_eq!(form, &BackendForm::from_record(&r));
    assert!(form.control, "the arming seeds the checkbox");
    assert!(errors.is_empty());
    s.cancel();
    assert_eq!(s, BackendEditorState::Closed);
}

/// A refused Save keeps the form OPEN with the errors stashed for rendering — the operator's
/// typing is not thrown away, and no update leaves the editor.
#[test]
fn a_refused_submit_keeps_the_form_open_with_errors() {
    let mut s = BackendEditorState::default();
    s.open_add();
    // Type a bad address into the open form.
    if let BackendEditorState::Add { form, .. } = &mut s {
        *form = valid_form("the CI box");
        form.address = "not-an-address".to_string();
    }
    let upd = submit(&mut s, &file_with(&[]));
    assert_eq!(upd, None, "a refused submit must produce no update");
    let BackendEditorState::Add { form, errors } = &s else {
        panic!("form must stay open, got {s:?}");
    };
    assert_eq!(form.name, "the CI box", "the typing survives the refusal");
    assert_eq!(errors, &vec![BackendFormError::AddressNotHostPort]);
}

/// A valid Add closes the editor and hands back the file with the record appended — and
/// never asks for a disconnect.
#[test]
fn a_valid_add_appends_and_closes() {
    let mut s = BackendEditorState::default();
    s.open_add();
    if let BackendEditorState::Add { form, .. } = &mut s {
        *form = valid_form("prod3");
    }
    let file = file_with(&["the CI box"]);
    let upd = submit(&mut s, &file).expect("valid add");
    assert_eq!(s, BackendEditorState::Closed);
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
fn edit_active_defers_record_saved_connection_untouched() {
    let live = rec("the CI box"); // the live connection's record
    let mut s = BackendEditorState::default();
    s.open_edit(&rec("the CI box"));
    if let BackendEditorState::Edit { form, .. } = &mut s {
        form.address = "the CI box.example:9999".to_string();
    }
    let file = file_with(&["the CI box", "prod3"]);
    let upd = submit(&mut s, &file).expect("valid edit");

    // The record IS saved…
    assert_eq!(upd.file.backends[0].address, "the CI box.example:9999");
    // …and the conn is NOT touched: no disconnect, nothing to hot-rewire with.
    assert!(!upd.disconnect_first, "an edit must never disconnect — next connect picks it up");
    // The UI's deferred-effect line shows exactly for the ACTIVE record's edit.
    assert!(edit_defers("the CI box", Some(&live)));
    assert!(!edit_defers("prod3", Some(&live)));
    assert!(!edit_defers("the CI box", None));
    assert!(!edit_defers("", Some(&crate::backend::connection::cli_observe_record("h:1"))));
}

/// A rename retargets the `active` pointer with the record, so the pointer keeps naming the
/// same record instead of dangling at the old name.
#[test]
fn an_edit_rename_retargets_the_active_pointer() {
    let mut file = file_with(&["the CI box", "prod3"]);
    file.active = Some("the CI box".to_string());
    let mut renamed = rec("the build runner");
    renamed.address = "h:1".to_string();
    let out = apply_edit(&file, "the CI box", renamed);
    assert_eq!(out.active.as_deref(), Some("the build runner"));
    assert_eq!(out.backends[0].name, "the build runner", "position preserved");
    assert_eq!(out.backends[1].name, "prod3");

    // A pointer at a DIFFERENT record does not move.
    file.active = Some("prod3".to_string());
    let out = apply_edit(&file, "the CI box", rec("the build runner"));
    assert_eq!(out.active.as_deref(), Some("prod3"));
}

/// While `backends.json` on disk is refused (an older build's file), a valid Add is refused too, with
/// the file's hint: the form stays open and no update leaves the editor, so the registry in memory
/// never holds a record the file cannot.
#[test]
fn a_refused_file_refuses_the_save_with_its_hint() {
    let hint = "backends.json was written by an older Vike Trader".to_string();
    let file = BackendRegistry { refused: Some(hint.clone()), ..Default::default() };
    let mut s = BackendEditorState::default();
    s.open_add();
    if let BackendEditorState::Add { form, .. } = &mut s {
        *form = valid_form("the CI box");
    }
    assert_eq!(submit(&mut s, &file), None, "no update while the file is refused");
    let BackendEditorState::Add { errors, .. } = &s else { panic!("the form stays open: {s:?}") };
    assert_eq!(errors, &vec![BackendFormError::FileNotRead(hint.clone())]);
    assert_eq!(errors[0].to_string(), hint, "the form shows the file's own hint");
}

/// DELETE NEEDS THE CONFIRM: Delete on a row only opens the are-you-sure — no update leaves
/// the editor, and Cancel walks it back with the registry untouched.
#[test]
fn delete_requires_confirmation_and_cancel_walks_back() {
    let mut s = BackendEditorState::default();
    s.request_delete("the CI box");
    assert_eq!(s, BackendEditorState::ConfirmDelete { name: "the CI box".to_string() });
    s.cancel();
    assert_eq!(s, BackendEditorState::Closed);
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

    let mut s = BackendEditorState::default();
    s.request_delete("the CI box");
    let upd = confirm_delete(&mut s, &file, Some(&live)).expect("confirmed");
    assert_eq!(s, BackendEditorState::Closed);
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
    let mut s = BackendEditorState::default();
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
    assert!(!delete_disconnects("", Some(&crate::backend::connection::cli_observe_record("h:1"))));
}

// ── the real save/load round-trip ───────────────────────────────────────────────────────

/// The editor's output round-trips through the REAL registry save/load: an add + an edit +
/// a delete driven through the state machine, written by `backend::registry`'s own saver
/// (sanitize + atomic write) and read back equal.
#[test]
fn editor_output_round_trips_through_the_real_save_load() {
    // Add "the CI box".
    let mut s = BackendEditorState::default();
    s.open_add();
    if let BackendEditorState::Add { form, .. } = &mut s {
        *form = valid_form("the CI box");
    }
    let file = submit(&mut s, &BackendRegistry::default()).expect("add").file;

    // Add "prod3", then edit it: rename + arm control.
    s.open_add();
    if let BackendEditorState::Add { form, .. } = &mut s {
        *form = valid_form("prod3");
    }
    let file = submit(&mut s, &file).expect("add 2").file;
    s.open_edit(&file.backends[1].clone());
    if let BackendEditorState::Edit { form, .. } = &mut s {
        form.name = "prod3-live".to_string();
        form.control = true;
    }
    let file = submit(&mut s, &file).expect("edit").file;

    // Delete "the CI box".
    s.request_delete("the CI box");
    let file = confirm_delete(&mut s, &file, None).expect("delete").file;

    // Through the real saver/loader.
    let tmp = tempfile::tempdir().expect("temp dir");
    let p = tmp.path().join("backends.json");
    crate::backend::registry::save_to(&file, &p).expect("save");
    let back = crate::backend::registry::load_from(&p).expect("load");
    assert_eq!(back, file, "editor output must survive the disk round-trip unchanged");
    assert_eq!(back.backends.len(), 1);
    assert_eq!(back.backends[0].name, "prod3-live");
    assert!(back.backends[0].control);
}
