//! **The per-field REMOVE** (decision 0144's GUI half), driven end to end through the real widget
//! against a throwaway store and a throwaway ledger.
//!
//! The owner's design, asserted as he stated it:
//!
//!  1. a Remove control sits beside each **held** field and beside no empty one;
//!  2. it takes **two clicks**: the first removes nothing and turns the control into
//!     "Confirm remove <KEY>", the second sends;
//!  3. **any other click** disarms it;
//!  4. afterwards the field is empty with the placeholder "not set", the status line says
//!     "Removed <KEY> — kept at rest, Save a value to add it back", and the store answers the name
//!     as absent while its OTHER keys are untouched;
//!  5. the ledger holds ONE `credential_retire` record naming the key, and no value.
//!
//! ⚠ The harness grid is derived ONCE (`AccountGrids::from_vars`), where the binary re-derives it
//! every frame from the store; so after a remove the control keeps its "held" mark here. What is
//! asserted afterwards is therefore the STORE, the status line and the field, never the control.

use std::collections::HashMap;

use egui::accesskit::Role;
use egui_kittest::kittest::NodeT;
use egui_kittest::{Harness, Node};
use vike_connections::{AccountGrids, CredentialWrite, connections_ui};
use vike_model::feed_status::ConnectionState;
use vike_model::scratch::ScratchDir;
use vike_secrets::change_journal::{CHANGES_SUBDIR, KIND_CREDENTIAL_RETIRE, Proc, month_file_name};
use vike_secrets::credentials::StoreHealth;

use crate::support::{TYPED_KEY, TYPED_SECRET, edit_buttons, nodes, text_fields, tree_text};

/// 2026-08-21T00:00:00Z — the injected instant, so the record lands in a known month file.
const T: i64 = 1_787_356_800_000;

const KEY: &str = "BINANCE_LIVE_API_KEY";
const SECRET: &str = "BINANCE_LIVE_API_SECRET";

/// A store that HOLDS binance's LIVE key and secret (and nothing else), written through the one
/// sanctioned writer.
fn seeded() -> ScratchDir {
    let r = ScratchDir::create_in(&std::env::temp_dir(), "vike-field-remove").expect("scratch");
    vike_secrets::create_store(Some(r.path().to_str().expect("utf-8 scratch root")))
        .expect("create the empty store");
    vike_secrets::save_credentials_to_store(
        r.path(),
        vike_secrets::Table::Credential,
        &[(KEY.to_string(), TYPED_KEY.to_string()), (SECRET.to_string(), TYPED_SECRET.to_string())],
        vike_secrets::UpsertIntent::Add,
    )
    .expect("seed the two keys");
    r
}

fn stored(r: &ScratchDir) -> HashMap<String, String> {
    vike_secrets::read_table(&vike_secrets::db_path_in(r.path()), vike_secrets::Table::Credential)
        .expect("the credential store reads")
        .into_map()
}

fn buttons_labelled<'t, 'h>(h: &'t Harness<'h, ()>, label: &str) -> Vec<Node<'t>> {
    let want = label.to_string();
    nodes(h, move |n| {
        let a = n.accesskit_node();
        a.role() == Role::Button && a.label().as_deref() == Some(want.as_str())
    })
}

/// The panel over the seeded store with binance's LIVE form open (binance is the roster's first
/// row, so it is the selected venue, and the LAST pencil is the LIVE tier).
fn open_live_form<'a>(r: &'a ScratchDir, proc: &'a Proc) -> Harness<'a, ()> {
    let grids = AccountGrids::from_vars(&stored(r));
    let live: HashMap<String, ConnectionState> = HashMap::new();
    let creds = CredentialWrite { settings_dir: r.path(), journal: None, proc, now_ms: T };
    let mut h = Harness::builder().with_size(egui::vec2(1000.0, 800.0)).build_ui(move |ui| {
        if vike_ui_theme::harness::type_ready(ui.ctx()) {
            connections_ui(ui, &grids, &live, &StoreHealth::Readable, creds, None);
        }
    });
    h.run();
    let pencils = edit_buttons(&h);
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

/// THE gate: held-only, two clicks, the empty "not set" field, the status line, the store, the ledger.
#[test]
fn a_held_field_is_removed_by_two_clicks_and_says_so() {
    let r = seeded();
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
        "the first held field's control now names its exact key: {}",
        tree_text(&h)
    );
    assert!(stored(&r).contains_key(KEY), "the arming click removes nothing");

    // …the SECOND click on the armed control sends.
    click_first(&mut h, &format!("Confirm remove {KEY}"));
    let after = stored(&r);
    assert!(!after.contains_key(KEY), "the removed key reads as absent: {:?}", after.keys());
    assert_eq!(
        after.get(SECRET).map(String::as_str),
        Some(TYPED_SECRET),
        "ONE key per remove: its neighbour is untouched"
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

    // (5) ONE record, of its own kind, naming the key and carrying neither value.
    let ledger = r.path().join("state").join(CHANGES_SUBDIR).join(month_file_name(T));
    let raw = std::fs::read_to_string(&ledger).expect("the change journal was written");
    assert_eq!(raw.lines().count(), 1, "one remove is one record: {raw}");
    let v: serde_json::Value = serde_json::from_str(raw.trim_end()).expect("one JSON object");
    assert_eq!(v["kind"], KIND_CREDENTIAL_RETIRE);
    assert_eq!(v["target"]["keys"], serde_json::json!([KEY]));
    assert!(!raw.contains(TYPED_KEY) && !raw.contains(TYPED_SECRET), "a value reached it: {raw}");
}

/// **Any other click disarms.** Arm the key, click Save (which has nothing to save and says so),
/// and the control is no longer a "Confirm remove" — and nothing was removed.
#[test]
fn any_other_click_disarms_an_armed_remove() {
    let r = seeded();
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
    assert!(stored(&r).contains_key(KEY), "…and nothing was removed");
}

/// An emptied field on Save is still SKIPPED — a Save never deletes. Remove the key, then Save
/// the (now empty) form: the store is exactly as the Remove left it, with the key absent.
#[test]
fn a_save_after_a_remove_does_not_bring_the_key_back_or_delete_anything_else() {
    let r = seeded();
    let proc = Proc::new("vike-test", 4711, "0.1.0");
    let mut h = open_live_form(&r, &proc);

    click_first(&mut h, "Remove");
    click_first(&mut h, &format!("Confirm remove {KEY}"));
    let before = stored(&r);
    assert!(!before.contains_key(KEY));

    click_first(&mut h, "Save");
    assert_eq!(stored(&r), before, "an empty Save writes nothing and deletes nothing");
    assert!(tree_text(&h).contains("nothing entered"), "…and says so: {}", tree_text(&h));
}
