//! **The per-field REMOVE** (decision 0144's GUI half), driven end to end through the real widget
//! against a throwaway store and a throwaway ledger.
//!
//! The owner's design, asserted as he stated it:
//!
//!  1. a Remove control sits beside each **held** field and beside no empty one;
//!  2. it takes **two clicks**: the first removes nothing and turns the control into
//!     "Confirm remove <FIELD>", the second sends;
//!  3. **any other click** disarms it;
//!  4. afterwards the field is empty with the placeholder "not set", the status line says
//!     "Removed <FIELD> — kept at rest, Save a value to add it back", and the store answers the
//!     field as absent while the account's OTHER fields are untouched;
//!  5. the ledger holds ONE `credential_retire` record naming the row — by the NAME the store
//!     stored it under, never one the panel composed — and no value.
//!
//! ⚠ The harness grid is read ONCE (`AccountGrids::read`), where the binary re-reads it every
//! frame; so after a remove the control keeps its "held" mark here. What is asserted afterwards is
//! therefore the STORE, the status line and the field, never the control.

use std::collections::HashMap;

use egui::accesskit::Role;
use egui_kittest::kittest::NodeT;
use egui_kittest::{Harness, Node};
use vike_change_journal::{CHANGES_SUBDIR, KIND_CREDENTIAL_RETIRE, Proc, month_file_name};
use vike_connections::{CredentialWrite, connections_ui};
use vike_model::AccountTier;
use vike_model::feed_status::ConnectionState;
use vike_paths::scratch::ScratchDir;
use vike_secrets::CredentialOwner;

use crate::support::{
    TYPED_KEY, TYPED_SECRET, create_account, edit_buttons, nodes, scratch_store, seed_fields,
    store_grids, stored_book, stored_field, text_fields, tree_text,
};

/// 2026-08-21T00:00:00Z — the injected instant, so the record lands in a known month file.
const T: i64 = 1_787_356_800_000;

const KEY: &str = "API_KEY";
const SECRET: &str = "API_SECRET";

/// A store whose binance DEFAULT account holds a LIVE row with its key and secret (and nothing
/// else): the account created with its tier, the two fields written by FIELD. Returns the row id
/// and the NAME the writer stored the key under.
fn seeded() -> (ScratchDir, i64, String) {
    let r = scratch_store("vike-field-remove");
    let id = create_account(r.path(), "binance", AccountTier::Live, None);
    let names = seed_fields(
        r.path(),
        &CredentialOwner::Account(id),
        &[(KEY, TYPED_KEY), (SECRET, TYPED_SECRET)],
    );
    (r, id, names[0].clone())
}

fn buttons_labelled<'t, 'h>(h: &'t Harness<'h, ()>, label: &str) -> Vec<Node<'t>> {
    let want = label.to_string();
    nodes(h, move |n| {
        let a = n.accesskit_node();
        a.role() == Role::Button && a.label().as_deref() == Some(want.as_str())
    })
}

/// The panel over the seeded store with binance's LIVE form open (binance is the roster's first
/// row, so it is the selected venue, and its one pencil is the LIVE row's).
fn open_live_form<'a>(r: &'a ScratchDir, proc: &'a Proc) -> Harness<'a, ()> {
    let grids = store_grids(r.path());
    let live: HashMap<String, ConnectionState> = HashMap::new();
    let creds = CredentialWrite { settings_dir: r.path(), journal: None, proc, now_ms: T };
    let mut h = Harness::builder().with_size(egui::vec2(1000.0, 800.0)).build_ui(move |ui| {
        if vike_ui_theme::harness::type_ready(ui.ctx()) {
            connections_ui(ui, &grids, &live, creds, None);
        }
    });
    h.run();
    let pencils = edit_buttons(&h);
    assert_eq!(pencils.len(), 1, "the default account's one row, at live");
    pencils.last().expect("an edit button").click();
    drop(pencils);
    h.run();
    h
}

fn click_first(h: &mut Harness<'_, ()>, label: &str) {
    let buttons = buttons_labelled(h, label);
    buttons.first().unwrap_or_else(|| panic!("no `{label}` button on screen")).click();
    drop(buttons);
    h.run();
}

/// THE gate: held-only, two clicks, the empty "not set" field, the status line, the store, the
/// ledger.
#[test]
fn a_held_field_is_removed_by_two_clicks_and_says_so() {
    let (r, id, key_name) = seeded();
    let proc = Proc::new("vike-test", 4711, "0.1.0");
    let mut h = open_live_form(&r, &proc);

    // (1) A Remove beside each HELD field — the key and the secret — and none beside the empty
    // passphrase and broker-code fields.
    assert_eq!(
        buttons_labelled(&h, "Remove").len(),
        2,
        "the two held fields offer a Remove, the two empty ones do not: {}",
        tree_text(&h)
    );
    assert!(buttons_labelled(&h, &format!("Confirm remove {KEY}")).is_empty());

    // (2) The FIRST click arms and removes nothing.
    click_first(&mut h, "Remove");
    assert_eq!(
        buttons_labelled(&h, &format!("Confirm remove {KEY}")).len(),
        1,
        "the first held field's control now names its exact field: {}",
        tree_text(&h)
    );
    assert!(stored_field(r.path(), id, KEY).is_some(), "the arming click removes nothing");

    // …the SECOND click on the armed control sends.
    click_first(&mut h, &format!("Confirm remove {KEY}"));
    assert_eq!(stored_field(r.path(), id, KEY), None, "the removed field reads as absent");
    assert_eq!(
        stored_field(r.path(), id, SECRET).as_deref(),
        Some(TYPED_SECRET),
        "ONE field per remove: its neighbour is untouched"
    );

    // (4) The status line, verbatim, and the field: empty, with the placeholder `not set`.
    let text = tree_text(&h);
    assert!(
        text.contains(&format!("Removed {KEY} — kept at rest, Save a value to add it back")),
        "{text}"
    );
    let fields = text_fields(&h);
    let not_set: Vec<_> = fields
        .iter()
        .filter(|f| {
            f.accesskit_node().placeholder().map(|p| p.to_string()).as_deref() == Some("not set")
        })
        .collect();
    assert_eq!(not_set.len(), 1, "exactly the removed field reads `not set`: {text}");
    assert_eq!(not_set[0].accesskit_node().value().unwrap_or_default(), "", "…and is empty");
    drop(fields);

    // (5) ONE record, of its own kind, naming the row by the store's own NAME and carrying
    // neither value.
    let ledger = r.path().join("state").join(CHANGES_SUBDIR).join(month_file_name(T));
    let raw = std::fs::read_to_string(&ledger).expect("the change journal was written");
    assert_eq!(raw.lines().count(), 1, "one remove is one record: {raw}");
    let v: serde_json::Value = serde_json::from_str(raw.trim_end()).expect("one JSON object");
    assert_eq!(v["kind"], KIND_CREDENTIAL_RETIRE);
    assert_eq!(v["target"]["keys"], serde_json::json!([key_name]));
    assert_eq!(v["target"]["tier"], "live", "the account's own tier word");
    assert!(!raw.contains(TYPED_KEY) && !raw.contains(TYPED_SECRET), "a value reached it: {raw}");
}

/// **Any other click disarms.** Arm the field, click Save (which has nothing to save and says so),
/// and the control is no longer a "Confirm remove" — and nothing was removed.
#[test]
fn any_other_click_disarms_an_armed_remove() {
    let (r, id, _) = seeded();
    let proc = Proc::new("vike-test", 4711, "0.1.0");
    let mut h = open_live_form(&r, &proc);

    click_first(&mut h, "Remove");
    assert_eq!(buttons_labelled(&h, &format!("Confirm remove {KEY}")).len(), 1);

    click_first(&mut h, "Save");
    h.run();
    assert!(
        buttons_labelled(&h, &format!("Confirm remove {KEY}")).is_empty(),
        "a click elsewhere must drop the arm: {}",
        tree_text(&h)
    );
    assert_eq!(buttons_labelled(&h, "Remove").len(), 2, "…back to two plain Removes");
    assert!(stored_field(r.path(), id, KEY).is_some(), "…and nothing was removed");
}

/// An emptied field on Save is still SKIPPED — a Save never deletes. Remove the field, then Save
/// the (now empty) form: the store is exactly as the Remove left it, with the field absent.
#[test]
fn a_save_after_a_remove_does_not_bring_the_field_back_or_delete_anything_else() {
    let (r, id, _) = seeded();
    let proc = Proc::new("vike-test", 4711, "0.1.0");
    let mut h = open_live_form(&r, &proc);

    click_first(&mut h, "Remove");
    click_first(&mut h, &format!("Confirm remove {KEY}"));
    let before = stored_book(r.path());
    assert_eq!(stored_field(r.path(), id, KEY), None);

    click_first(&mut h, "Save");
    assert_eq!(stored_book(r.path()), before, "an empty Save writes nothing and deletes nothing");
    assert!(tree_text(&h).contains("nothing entered"), "…and says so: {}", tree_text(&h));
}
