//! **The GUI credential editor's Save arm lands in the change journal** — driven end to end through
//! the real widget, against a throwaway store and a throwaway ledger.
//!
//! `crates/vike-connections/tests/panel/a11y_form.rs` says in its own header why it never clicks
//! **Save**. The settings directory is a [`vike_connections::CredentialWrite`] parameter, which is
//! what makes this file possible at all — and the parameter exists for a production reason, not
//! this one: the grid RENDERS from `vike-desktop`'s override-honouring read while Save once wrote
//! wherever a blind walk landed.
//!
//! What is gated here, in the order the record has to survive:
//!
//!  1. a Save produces **exactly one** `credential_write` record, with `origin: gui`;
//!  2. the record names the account's VENUE and its TIER word (`live` — the account row's, never a
//!     token read out of a name), the NAMES the store wrote and the true count;
//!  3. **no typed value reaches the ledger**, asserted on the raw bytes AND on every window of the
//!     secret long enough to matter (a `!contains(secret)` alone would pass a record that leaked all
//!     but the last character);
//!  4. the store REALLY was written — to the account ROW the form was opened for, by FIELD — so
//!     none of the above can pass because Save did nothing;
//!  5. `CredentialWrite::journal` does not govern this writer at all — see the second test.
//!
//! ⚠ **The append lives in `vike_secrets::save_owned_credentials`**, which `render_edit_form`'s
//! Save arm calls directly (decision 0156 item 6: an account's credential is written to its row).
//! It derives the ledger location itself, from the SAME settings directory the write lands in
//! (`<settings_dir>/state/changes`).

use std::collections::HashMap;

use egui::accesskit::Role;
use egui_kittest::kittest::NodeT;
use egui_kittest::{Harness, Node};
use vike_change_journal::{ChangeJournal, KIND_CREDENTIAL_WRITE, Proc};
use vike_connections::{CredentialWrite, connections_ui};
use vike_model::AccountTier;
use vike_model::feed_status::ConnectionState;
use vike_paths::scratch::ScratchDir;

use crate::support::{
    TYPED_KEY, TYPED_SECRET, create_account, nodes, password_fields, pencil, scratch_store,
    store_at, store_grids, stored_field,
};

/// 2026-08-21T00:00:00Z — the injected instant, so the record lands in a known month file.
const T: i64 = 1_787_356_800_000;

/// The shortest run of a secret this test refuses to find in the ledger.
///
/// Six rather than "the whole string": a record that wrote every character but the last would pass a
/// `contains(secret)` check, and a partial API secret is still an API secret.
const MIN_LEAK_WINDOW: usize = 6;

/// An EMPTY store holding one account row — binance, the default account, at LIVE — created with
/// its tier and no credential. Every field read back below was put there by the Save.
fn root() -> (ScratchDir, i64) {
    let r = scratch_store("vike-credwrite-journal");
    let id = create_account(r.path(), "binance", AccountTier::Live, None);
    (r, id)
}

fn buttons_labelled<'t, 'h>(h: &'t Harness<'h, ()>, label: &str) -> Vec<Node<'t>> {
    let want = label.to_string();
    nodes(h, move |n| {
        let a = n.accesskit_node();
        a.role() == Role::Button && a.label().as_deref() == Some(want.as_str())
    })
}

/// Drive the real widget: open binance's LIVE form, type into the first two fields, click Save.
///
/// Every step is its own frame. The harness queues input events and `run()` is what delivers them,
/// so focusing and typing in one batch would depend on egui's intra-frame event ordering — a
/// dependency this test has no reason to take.
fn save_binance_live(creds: CredentialWrite<'_>) {
    let grids = store_grids(creds.settings_dir);
    let live: HashMap<String, ConnectionState> = HashMap::new();
    let mut h = Harness::builder().with_size(egui::vec2(1000.0, 800.0)).build_ui(move |ui| {
        // The panel draws icons, whose family only the app's type binds.
        if vike_ui_theme::harness::type_ready(ui.ctx()) {
            connections_ui(ui, &grids, &live, creds, None);
        }
    });
    h.run();

    // The default account's one row is at LIVE, so its pencil is the only one.
    let pencils = buttons_labelled(&h, &pencil());
    assert_eq!(pencils.len(), 1, "binance's default account holds one row, at live");
    pencils.last().expect("an edit button").click();
    drop(pencils);
    h.run();

    // API Key, API Secret, Passphrase (optional) — leave the passphrase blank, so the record's
    // `count` has to come from what was ENTERED rather than from the form's field count.
    for (i, value) in [TYPED_KEY, TYPED_SECRET].into_iter().enumerate() {
        let fields = password_fields(&h);
        assert_eq!(fields.len(), 3, "the LIVE form offers three masked fields");
        fields[i].focus();
        drop(fields);
        h.run();

        let fields = password_fields(&h);
        fields[i].type_text(value);
        drop(fields);
        h.run();
    }

    let save = buttons_labelled(&h, "Save");
    assert_eq!(save.len(), 1, "exactly one Save button is on screen");
    save[0].click();
    drop(save);
    h.run();

    // The form CLOSED, which is `render_edit_form`'s success path and nothing else: the failure arm
    // leaves the fields up with an error message. So this is the widget's own verdict that the save
    // went through, independent of anything read off the disk below.
    assert!(
        password_fields(&h).is_empty(),
        "the form must close on a successful save — it is still open, so Save failed"
    );
}

/// The ledger must carry no run of `secret` at least [`MIN_LEAK_WINDOW`] characters long.
///
/// Every window, not just the whole string: the failure this guards against is a partial write, and
/// half of a live API secret is not half a problem.
fn assert_no_secret_window(raw: &str, secret: &str) {
    let chars: Vec<char> = secret.chars().collect();
    assert!(chars.len() >= MIN_LEAK_WINDOW, "the fixture is shorter than the window");
    for start in 0..=chars.len() - MIN_LEAK_WINDOW {
        for end in (start + MIN_LEAK_WINDOW)..=chars.len() {
            let window: String = chars[start..end].iter().collect();
            assert!(
                !raw.contains(&window),
                "the change journal carries a {}-character window of a typed credential \
                 ({window:?}). The record type takes no value parameter, so this means a value \
                 reached a cell that was supposed to hold a NAME.\nledger: {raw}",
                end - start
            );
        }
    }
}

/// THE gate: one Save, one record, the right actor — and not one character of what was typed.
#[test]
fn a_gui_credential_save_lands_as_one_gui_record_carrying_names_only() {
    let (r, id) = root();
    let state = r.path().join("state");
    let proc = Proc::new("vike-test", 4711, "0.1.0");
    // `journal: None` — the Save arm does not read it; it derives its own ledger below `state`
    // from the settings directory it is handed. The field stays on the type because the Venues
    // (arming) tab's UNRELATED `set_setting` write still reads it.
    let journal = ChangeJournal::in_state_dir(&state, proc.clone());

    save_binance_live(CredentialWrite {
        settings_dir: r.path(),
        journal: None,
        proc: &proc,
        now_ms: T,
    });

    // (4) first, because everything below is meaningless if Save did nothing: the account ROW
    // really holds what was typed. This is also the anti-vacuity half — a `connections_ui` that
    // silently stopped saving would leave an empty ledger AND an empty row.
    assert_eq!(stored_field(r.path(), id, "API_KEY").as_deref(), Some(TYPED_KEY));
    assert_eq!(stored_field(r.path(), id, "API_SECRET").as_deref(), Some(TYPED_SECRET));
    assert_eq!(
        stored_field(r.path(), id, "API_PASSPHRASE"),
        None,
        "a blank field means leave unchanged — it must not be written"
    );

    // (1) EXACTLY one record.
    let path = journal.file_for(T);
    let raw = std::fs::read_to_string(&path).expect("the change journal was written");
    assert_eq!(raw.lines().count(), 1, "one Save is one record: {raw}");

    // (3) NOT ONE CHARACTER of either value, anywhere in the raw bytes — asserted FIRST, before any
    // field is inspected. A cell-by-cell assertion that happened to run earlier would redden on the
    // same mutation and hide which property actually failed; this is the one that must never pass
    // for the wrong reason, so it is the one that reports.
    assert_no_secret_window(&raw, TYPED_KEY);
    assert_no_secret_window(&raw, TYPED_SECRET);

    let v: serde_json::Value = serde_json::from_str(raw.trim_end()).expect("one JSON object");
    assert_eq!(v["kind"], KIND_CREDENTIAL_WRITE);
    assert_eq!(v["outcome"], "applied");
    assert_eq!(v["actor"]["origin"], "gui", "the desktop editor is the GUI channel: {raw}");

    // (2) the venue, the account's tier word, the NAMES and the true count.
    assert_eq!(v["target"]["store"], vike_secrets::DB_FILE, "the store the write landed in");
    assert_eq!(v["target"]["venue"], "binance");
    assert_eq!(v["target"]["tier"], "live", "the account row's tier, never a name's token");
    assert_eq!(v["target"]["count"], 2, "two fields were filled in, not the form's three");
    let keys: Vec<&str> =
        v["target"]["keys"].as_array().unwrap().iter().map(|k| k.as_str().unwrap()).collect();
    // The names are the STORE's own — read back off its listing for this row, never composed here.
    let mut listed = store_at(r.path())
        .account_keys()
        .expect("the store reads")
        .expect("the store carries an account table")
        .remove(&id)
        .expect("the row owns names now")
        .names;
    listed.sort();
    let mut recorded: Vec<String> = keys.iter().map(|k| (*k).to_string()).collect();
    recorded.sort();
    assert_eq!(recorded, listed, "the record names exactly the rows the Save wrote");
}

/// (5) `CredentialWrite::journal` is IGNORED by this Save arm: `journal: None` still records,
/// because `vike_secrets::save_owned_credentials` derives its own ledger from the settings
/// directory the store write lands in — a settings directory always answers its own state
/// directory, so there is no "boot found no project" case left for THIS writer to go silent on.
#[test]
fn journal_none_no_longer_silences_this_writer_because_it_derives_its_own_ledger() {
    let (r, id) = root();
    let proc = Proc::new("vike-test", 4711, "0.1.0");

    save_binance_live(CredentialWrite {
        settings_dir: r.path(),
        journal: None,
        proc: &proc,
        now_ms: T,
    });

    assert_eq!(
        stored_field(r.path(), id, "API_KEY").as_deref(),
        Some(TYPED_KEY),
        "the credential store was still written"
    );

    let raw = std::fs::read_to_string(
        r.path().join("state").join("changes").join(vike_change_journal::month_file_name(T)),
    )
    .expect(
        "the write derives its own ledger from the settings directory, regardless of `journal`",
    );
    assert_eq!(raw.lines().count(), 1, "one Save is one record: {raw}");
}
