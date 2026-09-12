//! **The GUI credential editor's Save arm lands in the change journal** — driven end to end through
//! the real widget, against a throwaway store and a throwaway ledger.
//!
//! `crates/vike-connections/tests/connections_a11y.rs` says in its own header why it never clicks
//! **Save**: before this change `render_edit_form` resolved the credential store for itself, from
//! the `$VIKE_SETTINGS_DIR`-blind walk, so clicking Save in a test would have rewritten whoever's
//! `secrets.env` the working directory sat above. The store is now a
//! [`vike_connections::CredentialWrite`] parameter, which is what makes this file possible at all —
//! and the parameter exists for a production reason, not this one: the grid RENDERS from
//! `vike-app`'s override-honouring credential read while Save wrote wherever the blind walk landed.
//!
//! What is gated here, in the order the record has to survive:
//!
//!  1. a Save produces **exactly one** `credential_write` record, with `origin: gui`;
//!  2. the record names the KEYS and the true count — that is the whole reason the channel exists;
//!  3. **no typed value reaches the ledger**, asserted on the raw bytes AND on every window of the
//!     secret long enough to matter (a `!contains(secret)` alone would pass a record that leaked all
//!     but the last character);
//!  4. the store REALLY was written, so none of the above can pass because Save did nothing;
//!  5. a journal-less surface writes nothing and invents no directory.

use std::collections::HashMap;
use std::path::Path;

use egui::accesskit::Role;
use egui_kittest::kittest::NodeT;
use egui_kittest::{Harness, Node};
use vike_connections::{
    AccountGrids, ConnectionState, CredentialWrite, VenueCredStatus, connections_ui,
};
use vike_model::change_journal::{ChangeJournal, KIND_CREDENTIAL_WRITE, Proc};
use vike_model::scratch::ScratchDir;

/// The glyph `status_cell` labels its edit button with.
const PENCIL: &str = "\u{270E}";

/// 2026-08-21T00:00:00Z — the injected instant, so the record lands in a known month file.
const T: i64 = 1_787_356_800_000;

/// The two values typed into the LIVE form.
///
/// ⚠ Chosen to share no six-character run with anything a `credential_write` line legitimately
/// contains — no `key`, no `secret`, no `live`, no venue name, no digits that appear in [`T`]. That
/// is what lets [`assert_no_secret_window`] use a small window without false positives, and it is
/// the difference between a leak test and a coincidence test.
const TYPED_KEY: &str = "zqxjvw7413mfbphgnd8256wu";
const TYPED_SECRET: &str = "pfgzmwqx9042hvbjntdu6531";

/// The shortest run of a secret this test refuses to find in the ledger.
///
/// Six rather than "the whole string": a record that wrote every character but the last would pass a
/// `contains(secret)` check, and a partial API secret is still an API secret.
const MIN_LEAK_WINDOW: usize = 6;

fn root() -> ScratchDir {
    // The system temp directory is legitimate in a test — `crates/vike-ops/tests/system_temp_gate.rs`
    // scopes itself to production code. `ScratchDir` is unique per process and self-deleting on the
    // panic path, the same dogfooding `vike_model::change_journal`'s own tests use.
    ScratchDir::create_in(&std::env::temp_dir(), "vike-credwrite-journal").expect("scratch root")
}

fn nodes<'t, 'h>(h: &'t Harness<'h, ()>, pred: impl Fn(&Node<'t>) -> bool) -> Vec<Node<'t>> {
    h.root().children_recursive().filter(|n| pred(n)).collect()
}

fn buttons_labelled<'t, 'h>(h: &'t Harness<'h, ()>, label: &str) -> Vec<Node<'t>> {
    let want = label.to_string();
    nodes(h, move |n| {
        let a = n.accesskit_node();
        a.role() == Role::Button && a.label().as_deref() == Some(want.as_str())
    })
}

fn password_fields<'t, 'h>(h: &'t Harness<'h, ()>) -> Vec<Node<'t>> {
    nodes(h, |n| n.accesskit_node().role() == Role::PasswordInput)
}

/// Drive the real widget: open binance's LIVE form, type into the first two fields, click Save.
///
/// Every step is its own frame. The harness queues input events and `run()` is what delivers them,
/// so focusing and typing in one batch would depend on egui's intra-frame event ordering — a
/// dependency this test has no reason to take.
fn save_binance_live(creds: CredentialWrite<'_>) {
    let grids = AccountGrids::single(vec![VenueCredStatus {
        venue: "binance".to_string(),
        sim: false,
        demo: false,
        live: false,
    }]);
    let live: HashMap<String, ConnectionState> = HashMap::new();
    let mut h = Harness::builder().with_size(egui::vec2(1000.0, 800.0)).build_ui(move |ui| {
        connections_ui(ui, &grids, &live, creds, None);
    });
    h.run();

    // Row order is Sim, Demo, Live — the LAST ✎ opens the LIVE form.
    let pencils = buttons_labelled(&h, PENCIL);
    assert_eq!(pencils.len(), 3, "binance stores keys at all three tiers");
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
    let r = root();
    let store = r.path().join("secrets.env");
    let state = r.path().join("state");
    let journal = ChangeJournal::in_state_dir(&state, Proc::new("vike-test", 4711, "0.1.0"));

    save_binance_live(CredentialWrite { store: &store, journal: Some(&journal), now_ms: T });

    // (4) first, because everything below is meaningless if Save did nothing: the STORE really
    // holds what was typed. This is also the anti-vacuity half — a `connections_ui` that silently
    // stopped saving would leave an empty ledger AND an empty store, and only this assertion can
    // tell that apart from "the journal wiring was removed".
    let saved = std::fs::read_to_string(&store).expect("the credential store was written");
    assert!(saved.contains(&format!("BINANCE_LIVE_API_KEY={TYPED_KEY}")), "{saved}");
    assert!(saved.contains(&format!("BINANCE_LIVE_API_SECRET={TYPED_SECRET}")), "{saved}");
    assert!(
        !saved.contains("BINANCE_LIVE_API_PASSPHRASE"),
        "a blank field means leave unchanged — it must not be written: {saved}"
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

    // (2) the NAMES and the true count.
    assert_eq!(v["target"]["store"], "secrets.env");
    assert_eq!(v["target"]["venue"], "binance");
    assert_eq!(v["target"]["tier"], "LIVE", "the LIVE cell's ✎ opened the LIVE form");
    assert_eq!(v["target"]["count"], 2, "two fields were filled in, not the form's three");
    let keys: Vec<&str> =
        v["target"]["keys"].as_array().unwrap().iter().map(|k| k.as_str().unwrap()).collect();
    assert_eq!(keys, vec!["BINANCE_LIVE_API_KEY", "BINANCE_LIVE_API_SECRET"]);
}

/// (5) A journal-less surface writes NOTHING and invents no directory — the credential still saves.
///
/// The honest degradation for a root whose boot walk found no project. Asserted against a state
/// directory that is never created, so "wrote nothing" and "wrote somewhere else" are
/// distinguishable: an invented path would leave a directory behind.
#[test]
fn a_journal_less_editor_still_saves_and_records_nothing() {
    let r = root();
    let store = r.path().join("secrets.env");

    save_binance_live(CredentialWrite { store: &store, journal: None, now_ms: T });

    let saved = std::fs::read_to_string(&store).expect("the credential store was still written");
    assert!(saved.contains(&format!("BINANCE_LIVE_API_KEY={TYPED_KEY}")), "{saved}");

    // Nothing else appeared beside it — no `changes/`, no `state/`, no ledger anywhere under the
    // scratch root.
    let siblings: Vec<String> = std::fs::read_dir(r.path())
        .expect("scratch root")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(siblings, vec!["secrets.env".to_string()], "a journal-less save invented a path");
    assert!(!Path::new(&r.path().join("state")).exists());
}
