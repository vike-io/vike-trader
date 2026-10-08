//! **The Connections panel's account dimension**, driven through the real widget
//! (`vike_connections::connections_ui`) and asserted over the accessibility tree.
//!
//! `crates/vike-connections/tests/key_shapes/account_grids.rs` gates the pure enumeration; this file gates the
//! SCREEN and the WRITE — which chips exist, which account a form belongs to, what the form writes,
//! and what an invalid label does before anything reaches disk.
//!
//! # The contract this change is held to
//!
//! **A store with no labelled account renders and behaves exactly as it did.** That is
//! [`a_store_with_no_labelled_account_renders_the_panel_that_shipped`], which pins the panel's
//! entire button set and the default account's form placeholders, plus the two files that were
//! already gating this surface and pass UNCHANGED except for their harness constructor
//! (`connections_a11y.rs`, `credential_write_journal.rs`) — the latter being the end-to-end proof
//! that a default-account Save still writes the unlabelled key names and journals the unlabelled
//! record.
//!
//! # Why the labelled fixtures live here rather than under `src/`
//!
//! Same reason as `crates/vike-connections/tests/key_shapes/account_status.rs`: the settings-registry scanner
//! harvests env-var-shaped literals out of `src/` and demands a declaration for each, and the
//! labelled key space deliberately has none. That file's header carries the full argument.
//!
//! ⚠ Where a test clicks **Save**, it writes to a `vike_model::scratch::ScratchDir`, never to a
//! resolved store — `connections_ui` takes its store as a `CredentialWrite` parameter, which is
//! what makes that safe (see `credential_write_journal.rs`'s header).

use std::collections::HashMap;
use std::path::Path;

use egui::accesskit::Role;
use egui_kittest::kittest::NodeT;
use egui_kittest::{Harness, Node};
use vike_connections::{
    AccountGrids, CredentialWrite, StoreHealth, VenueCredStatus, connections_ui,
};
use vike_model::accounts::account_keys::{ACCOUNT_SEPARATOR, AccountLabel};
use vike_model::feed_status::ConnectionState;
use vike_model::scratch::ScratchDir;

use crate::support::{
    NEVER_WRITTEN_STORE, PanelHarness, TYPED_KEY, TYPED_SECRET, dry_creds, nodes, password_fields,
    pencil, test_proc, tree_text,
};

/// The glyph the SELECTED account chip is marked with.
const DOT: &str = "\u{25CF}";

fn label(text: &str) -> AccountLabel {
    AccountLabel::parse(text).expect("a legal label")
}

fn binance_row(configured: bool) -> VenueCredStatus {
    VenueCredStatus { venue: "binance".to_string(), sim: false, demo: false, live: configured }
}

/// ONE venue's row, so a count of buttons is a count of THAT venue's affordances.
fn harness(grids: AccountGrids, creds: CredentialWrite<'static>) -> Harness<'static, ()> {
    PanelHarness { size: egui::vec2(1200.0, 900.0), creds, run: false, ..PanelHarness::new(grids) }
        .build()
}

/// The harness above **driven the way the production caller drives a preselection**: the pending
/// account is `take`n, so it is offered on the first frame and never again.
///
/// ⚠ The `take` is the point, and it belongs to the caller:
/// `crates/vike-app-core/src/ui/tool_views/data.rs`'s `data_body` (its `DataDest::Credentials`
/// arm) takes `ToolView::connections_account` rather than reading it. Reproducing the caller's
/// shape here is what lets the ONE-SHOT property be tested at all: `connections_ui` itself honours
/// whatever it is handed every frame it is handed one (see its doc), so a harness that re-offered
/// the same `Some` forever would be testing a caller nobody has.
fn harness_preselect(
    grids: AccountGrids,
    creds: CredentialWrite<'static>,
    preselect: AccountLabel,
) -> Harness<'static, ()> {
    let mut pending = Some(preselect);
    PanelHarness {
        size: egui::vec2(1200.0, 900.0),
        creds,
        // The type is bound on the first frame, which draws nothing — so the `take` happens on
        // the first frame that DRAWS, as the production caller's does.
        preselect: Box::new(move || pending.take()),
        run: false,
        ..PanelHarness::new(grids)
    }
    .build()
}

/// A single-account panel — the shape a box with no labelled account has.
fn default_only() -> Harness<'static, ()> {
    harness(AccountGrids::single(vec![binance_row(false)]), dry_creds())
}

/// A panel with one labelled account beside the default one.
fn with_alt() -> Harness<'static, ()> {
    harness(
        AccountGrids::new(vec![binance_row(true)], vec![(label("ALT"), vec![binance_row(false)])]),
        dry_creds(),
    )
}

/// The same panel, opened with `ALT` already selected — the QA capture arm's configuration.
fn with_alt_preselected() -> Harness<'static, ()> {
    harness_preselect(
        AccountGrids::new(vec![binance_row(true)], vec![(label("ALT"), vec![binance_row(false)])]),
        dry_creds(),
        label("ALT"),
    )
}

fn buttons<'t, 'h>(h: &'t Harness<'h, ()>) -> Vec<Node<'t>> {
    nodes(h, |n| n.accesskit_node().role() == Role::Button)
}

fn button_labels(h: &Harness<'_, ()>) -> Vec<String> {
    buttons(h).iter().map(|n| n.accesskit_node().label().unwrap_or_default().to_string()).collect()
}

fn button_labelled<'t, 'h>(h: &'t Harness<'h, ()>, want: &str) -> Node<'t> {
    let wanted = want.to_string();
    let mut found = nodes(h, move |n| {
        let a = n.accesskit_node();
        a.role() == Role::Button && a.label().as_deref() == Some(wanted.as_str())
    });
    assert_eq!(found.len(), 1, "exactly one button labelled {want:?} must be on screen");
    found.remove(0)
}

fn text_inputs<'t, 'h>(h: &'t Harness<'h, ()>) -> Vec<Node<'t>> {
    nodes(h, |n| n.accesskit_node().role() == Role::TextInput)
}

/// Every OPEN form field's placeholder — the key name it writes — in field order.
///
/// ⚠ **Both roles, not just `PasswordInput`.** This walked the masked fields alone while every
/// field was masked; it is not any more. `vike_connections::keys::key_sensitivity` renders a name
/// (a server, an account number, an attribution code) as an ordinary `Role::TextInput` so an
/// operator can read it back, and a helper that kept looking only at password inputs would have
/// gone quietly blind to exactly the fields this panel most recently grew — asserting a form's key
/// list while silently dropping part of it.
fn form_fields<'t, 'h>(h: &'t Harness<'h, ()>) -> Vec<Node<'t>> {
    nodes(h, |n| matches!(n.accesskit_node().role(), Role::PasswordInput | Role::TextInput))
}

fn placeholders(h: &Harness<'_, ()>) -> Vec<String> {
    form_fields(h)
        .iter()
        .map(|f| f.accesskit_node().placeholder().unwrap_or_default().to_string())
        .collect()
}

/// Open a venue's LIVE credential form — the LAST edit pencil, row order being Sim, Demo, Live.
fn open_live_form<'h>(h: &mut Harness<'h, ()>) {
    let pencils = nodes(h, |n| {
        let a = n.accesskit_node();
        a.role() == Role::Button && a.label().as_deref() == Some(pencil().as_str())
    });
    assert_eq!(pencils.len(), 3, "binance stores keys at all three tiers");
    pencils.last().expect("an edit button").click();
    drop(pencils);
    h.run();
}

/// Click a button, deliver the click.
fn click<'h>(h: &mut Harness<'h, ()>, want: &str) {
    {
        // Scoped so the tree borrow ends before `run` needs the harness mutably.
        button_labelled(h, want).click();
    }
    h.run();
}

/// **THE unchanged-panel gate.** With no labelled account the whole button set is the one that
/// shipped plus a single `Add account`, the default account is the one selected, no form is open,
/// and the LIVE form's placeholders are the UNLABELLED key names.
///
/// Reddens on a chip appearing for an account that does not exist, on the selector defaulting to
/// anything but the default account, and — the one that matters — on `account_fields` composing a
/// label into a default-account key name.
#[test]
fn a_store_with_no_labelled_account_renders_the_panel_that_shipped() {
    let mut h = default_only();
    h.run();

    let mut labels = button_labels(&h);
    labels.sort();
    assert_eq!(
        labels,
        vec!["Add account".to_string(), pencil(), pencil(), pencil()],
        "the panel offers the three edit affordances it always did, and one way to name an account"
    );
    let text = tree_text(&h);
    assert!(text.contains(&format!("{DOT} default")), "the default account is selected: {text}");
    assert!(
        !text.contains(ACCOUNT_SEPARATOR),
        "no labelled key name may reach a single-account panel: {text}"
    );
    assert!(password_fields(&h).is_empty(), "no credential form is open");
    assert!(text_inputs(&h).is_empty(), "the Add-account field is closed until it is asked for");

    open_live_form(&mut h);
    assert_eq!(
        placeholders(&h),
        vec![
            "BINANCE_LIVE_API_KEY",
            "BINANCE_LIVE_API_SECRET",
            "BINANCE_LIVE_API_PASSPHRASE",
            // ⚠ The VENUE-WIDE attribution tail, appended to every mechanised venue's LIVE cell.
            // It carries no account label in EITHER account's form — `vike_connections::view`'s
            // `attribution_fields` argues why, and the labelled twin of this test below is where
            // that is visible: it is the one placeholder there without an `__ALT` suffix.
            "BINANCE_BROKER_CODE",
        ],
        "the default account's form writes the key names it always wrote, plus the venue-wide \
         attribution code"
    );
}

/// A labelled account is one click away, and ITS form writes ITS key names.
///
/// Reddens on `account_fields` ignoring the selection (the placeholders lose their suffix), and on
/// the chip strip rendering the selected account as a button (which would make "which account am I
/// editing" unanswerable from the tree).
#[test]
fn a_labelled_account_is_selectable_and_its_form_writes_labelled_key_names() {
    let mut h = with_alt();
    h.run();
    assert!(tree_text(&h).contains(&format!("{DOT} default")), "default starts selected");

    click(&mut h, "ALT");
    let text = tree_text(&h);
    assert!(text.contains(&format!("{DOT} ALT")), "ALT is now the selected account: {text}");
    assert!(
        button_labels(&h).iter().any(|l| l == "default"),
        "…and the default account became the clickable chip"
    );

    open_live_form(&mut h);
    assert_eq!(
        placeholders(&h),
        vec![
            format!("BINANCE_LIVE_API_KEY{ACCOUNT_SEPARATOR}ALT"),
            format!("BINANCE_LIVE_API_SECRET{ACCOUNT_SEPARATOR}ALT"),
            format!("BINANCE_LIVE_API_PASSPHRASE{ACCOUNT_SEPARATOR}ALT"),
            // ⚠ …every field EXCEPT the venue-wide attribution code, which is deliberately
            // unlabelled: `vike_bridge_core::credentials::attribution_code_from` takes no
            // `AccountLabel` and reads the bare name, so a labelled spelling would be a key
            // nothing in this tree reads.
            "BINANCE_BROKER_CODE".to_string(),
        ],
        "every CREDENTIAL field of the ALT form writes an ALT key, and the venue-wide code does not"
    );
}

/// **The QA capture arm's seam: a PRESELECTED account is selected on the first frame, unclicked.**
///
/// `scripts/qa_shots.sh`'s `05b-connections-account` capture exists because two things this panel
/// renders only for a labelled account — the chip strip and the wrapping removal-instruction line
/// carrying the store path — are reachable by no headless layout test and, before the preselect
/// parameter, by no capture either: nothing could choose an account, and a screenshot harness
/// cannot click.
///
/// ⚠ It asserts the KEY NAMES as well as the chip, and that is the "right answer for the wrong
/// reason" guard. A preselect that moved the chip but not `EditState::account` would draw a
/// perfectly convincing capture of the ALT account over the DEFAULT account's key space — the
/// exact failure the sheet exists to make visible, rendered invisible. The placeholders are the
/// only thing on screen that can tell them apart.
#[test]
fn a_preselected_account_is_the_selected_chip_and_owns_the_form_without_a_click() {
    let mut h = with_alt_preselected();
    h.run();

    let text = tree_text(&h);
    assert!(text.contains(&format!("{DOT} ALT")), "ALT is selected on the first frame: {text}");
    assert!(
        !text.contains(&format!("{DOT} default")),
        "…and the default account is NOT the selected chip: {text}"
    );
    assert!(
        button_labels(&h).iter().any(|l| l == "default"),
        "the default account is still one click away — a preselect must not remove a chip"
    );

    open_live_form(&mut h);
    assert_eq!(
        placeholders(&h),
        vec![
            format!("BINANCE_LIVE_API_KEY{ACCOUNT_SEPARATOR}ALT"),
            format!("BINANCE_LIVE_API_SECRET{ACCOUNT_SEPARATOR}ALT"),
            format!("BINANCE_LIVE_API_PASSPHRASE{ACCOUNT_SEPARATOR}ALT"),
            // The venue-wide attribution code, unlabelled in every account's form — see the twin
            // assertion above.
            "BINANCE_BROKER_CODE".to_string(),
        ],
        "the preselection reached the key composition, not just the chip's paint"
    );
}

/// ⚠ **A preselection is a ONE-SHOT, so it cannot make the chip strip inert.**
///
/// The panel's selection lives in the widget's egui temp memory, which is re-read every frame — so
/// a caller that kept handing the same `Some` down would re-select that account after every click,
/// and the chips would land, repaint, and revert with nothing on screen explaining why. The
/// production caller (`vike_app_core::ui::tool_views::data_tool_content`'s `data_body`, on
/// `DataDest::Credentials`) answers that by `take`ing `ToolView::connections_account`, and
/// [`harness_preselect`] reproduces exactly that.
///
/// Reddens on the `take` becoming a read at either end.
#[test]
fn a_preselected_account_does_not_survive_the_operator_clicking_another_chip() {
    let mut h = with_alt_preselected();
    h.run();
    assert!(tree_text(&h).contains(&format!("{DOT} ALT")), "the preselect took effect at all");

    click(&mut h, "default");
    let text = tree_text(&h);
    assert!(
        text.contains(&format!("{DOT} default")),
        "the operator's click must stand on every later frame: {text}"
    );
    assert!(!text.contains(&format!("{DOT} ALT")), "…and must not be re-overridden: {text}");
}

/// ⚠ **Switching account CLOSES an open credential form.**
///
/// The buffers were typed against the account that was selected when the form opened; carrying them
/// across a switch would compose those characters into the NEW account's key names on Save — a live
/// key written into the wrong account, with nothing on screen having moved. Reddens on
/// `EditState::select_account` dropping its `close()`.
#[test]
fn switching_account_closes_an_open_credential_form() {
    let mut h = with_alt();
    h.run();
    open_live_form(&mut h);
    assert_eq!(password_fields(&h).len(), 3, "the default account's LIVE form is open");

    click(&mut h, "ALT");
    assert!(
        password_fields(&h).is_empty(),
        "the form must close on an account switch — it is still open, so its buffers now belong \
         to an account they were not typed for"
    );
}

/// Type a label into the Add-account form, click Create, and return what the tree says afterwards.
fn offer_label(text: &str) -> String {
    let mut h = default_only();
    h.run();
    click(&mut h, "Add account");

    let fields = text_inputs(&h);
    assert_eq!(fields.len(), 1, "the Add-account form offers exactly one field");
    fields[0].focus();
    drop(fields);
    h.run();
    let fields = text_inputs(&h);
    fields[0].type_text(text);
    drop(fields);
    h.run();

    click(&mut h, "Create");
    tree_text(&h)
}

/// **An invalid label is refused BEFORE anything is written**, with the validator's own reason.
///
/// The message is `vike_model::accounts::account_keys::AccountKeyError`'s `Display` rather than a second
/// wording maintained here, so a rule change cannot leave the screen explaining the old rule.
/// Reddens on `EditState::create_account` accepting an unvalidated string.
#[test]
fn an_invalid_account_label_is_refused_with_the_validators_own_reason() {
    // Lowercase: refused rather than repaired, deliberately — `AccountLabel::parse` says why.
    let text = offer_label("alt");
    assert!(text.contains("is neither"), "the char refusal must name the offending byte: {text}");
    assert!(text.contains(&format!("{DOT} default")), "…and no account was selected: {text}");
    assert!(!text.contains(&format!("{DOT} alt")), "the refused label must not become a chip");

    // The reserved spelling for the account an unlabelled key already addresses.
    let text = offer_label("DEFAULT");
    assert!(text.contains("reserved"), "the reserved-label refusal must be shown: {text}");

    // Longer than the grammar's ceiling.
    let text = offer_label("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
    assert!(text.contains("at most"), "the length refusal must be shown: {text}");

    // Nothing typed at all.
    assert!(
        !Path::new(NEVER_WRITTEN_STORE).exists(),
        "not one of these refusals may have touched a store"
    );
}

/// A VALID label is accepted, selected, marked as holding nothing yet — and writes NOTHING.
///
/// An account exists when its first credential is saved; there is no registry for a Create to add a
/// row to, which is what keeps the screen and the store from ever disagreeing about what exists.
#[test]
fn a_valid_label_is_accepted_selects_the_account_and_writes_nothing() {
    let text = offer_label("HEDGE");
    assert!(text.contains(&format!("{DOT} HEDGE (new)")), "the new account is selected: {text}");
    assert!(
        text.contains("holds no credentials in this store yet"),
        "a not-yet-filled-in account must say so rather than showing bare empty dots: {text}"
    );
    assert!(!Path::new(NEVER_WRITTEN_STORE).exists(), "naming an account writes nothing");
}

/// **The panel deletes no CREDENTIAL, and removal is a ROW act behind a typed confirm.**
///
/// ⚠ **This test read `the_account_panel_offers_no_removal_affordance` and asserted an
/// INSTRUCTION.** The panel used to print *"To REMOVE account ALT, delete its `__ALT` lines from
/// {store} by hand"* — and on a migrated box that instruction is WRONG: `docs/decisions/0054`
/// stopped reading `secrets.env`, and those lines are `credential` ROWS in a database, so it named
/// a file nothing loads. `docs/decisions/0065` is the record that replaced it.
///
/// **What survives unchanged is the rule it was protecting**, and it is now enforced one layer
/// down rather than by having no button: nothing in this workspace deletes, moves, truncates or
/// wholesale-rewrites the credential store, and `vike_secrets::edit_account` REFUSES to delete an
/// account row that still owns credentials, naming them by key NAME. So what this test pins now is
/// the VOCABULARY — no affordance here claims to delete a credential or an "account" as a whole —
/// and the absence of any row act on a store that has no rows.
///
/// The typed-confirm half is `EditState::remove_confirm`: the Remove button ARMS a box the
/// operator must type the row id into, and removes nothing by itself. Its siblings require the
/// same (`vike-cli secrets account remove --confirm N`, and the node wire's
/// `AccountRequest::confirm`).
#[test]
fn the_panel_offers_no_credential_deleting_affordance() {
    let mut h = with_alt();
    h.run();
    click(&mut h, "ALT");

    let labels = button_labels(&h);
    // ⚠ "Remove" is deliberately NOT in this list any more, and the difference is the whole point:
    // a per-ROW Remove is a legitimate affordance (it deletes an `account` row, refuses while that
    // row owns credentials, and needs a typed confirm), while every word below claims to delete
    // the operator's KEYS and none of them may ever appear.
    for forbidden in
        ["Delete", "Delete account", "Remove account", "Forget", "Delete credentials", "Reset"]
    {
        assert!(
            !labels.iter().any(|l| l == forbidden),
            "a {forbidden:?} button would have to delete the operator's only copy of live venue \
             keys: {labels:?}"
        );
    }
    let text = tree_text(&h);
    assert!(
        !text.contains("by hand"),
        "the hand-edit instruction is a tombstone and must not come back — it named a file a \
         migrated box no longer reads: {text}"
    );
    assert!(
        text.contains("nothing here deletes a credential"),
        "the panel must still say the rule out loud: {text}"
    );
}

/// **THE write gate: a labelled Save writes ONLY that account's keys, and leaves the default
/// account's line byte-identical.**
///
/// This is the whole of the "the machinery exists — do not rebuild it" claim, proven against a real
/// store: `vike_secrets::save_credentials_to_store` is key-name agnostic, so the account dimension
/// is carried entirely by the names `account_fields` composes, and the row upsert does the rest.
#[test]
fn a_labelled_save_writes_only_that_accounts_keys_and_preserves_the_default_accounts() {
    let root =
        ScratchDir::create_in(&std::env::temp_dir(), "vike-account-editor").expect("scratch root");
    let store = root.path().join("secrets.env");
    // ⚠ The store is the settings DATABASE beside `store` (the GUI derives the settings directory
    // from that path), brought into being the one way a store comes into being — `migrate --init`
    // (`vike_secrets::migrate`) — and SEEDED through the one sanctioned writer, so the fixture
    // cannot encode a store shape the app could not have produced. (It seeded a `secrets.env` through
    // the byte-preserving file writer until the credential FILE store was removed on 2026-10-07.)
    //
    // What IS asserted is this file's own question — that a LABELLED save leaves the DEFAULT
    // account's keys, and an unrelated key, exactly where they were.
    let dir = root.path().to_str().expect("utf-8 scratch root");
    vike_secrets::migrate(
        Some(dir),
        vike_model::credential_keys::is_platform_key,
        &vike_bridge_core::credentials::classify_credential_name,
        vike_secrets::WhenNothingToCarry::CreateEmptyStore,
    )
    .expect("create the empty store");
    vike_secrets::save_credentials_to_store(
        root.path(),
        vike_secrets::Table::Credential,
        &[
            ("BINANCE_LIVE_API_KEY".to_string(), "default-account-key-untouched".to_string()),
            ("BINANCE_LIVE_API_SECRET".to_string(), "default-account-secret-untouched".to_string()),
            ("UNRELATED_KEY".to_string(), "keep-me".to_string()),
        ],
        Some(&vike_bridge_core::credentials::classify_credential_name),
    )
    .expect("seed the store");

    let creds = CredentialWrite { store: &store, journal: None, proc: test_proc(), now_ms: 0 };
    let live: HashMap<String, ConnectionState> = HashMap::new();
    let grids =
        AccountGrids::new(vec![binance_row(true)], vec![(label("ALT"), vec![binance_row(false)])]);
    let mut h = Harness::builder().with_size(egui::vec2(1200.0, 900.0)).build_ui(move |ui| {
        if vike_ui_theme::harness::type_ready(ui.ctx()) {
            connections_ui(ui, &grids, &live, &StoreHealth::Readable, creds, None);
        }
    });
    h.run();

    button_labelled(&h, "ALT").click();
    h.run();
    open_live_form(&mut h);

    for (i, value) in [TYPED_KEY, TYPED_SECRET].into_iter().enumerate() {
        let fields = password_fields(&h);
        fields[i].focus();
        drop(fields);
        h.run();
        let fields = password_fields(&h);
        fields[i].type_text(value);
        drop(fields);
        h.run();
    }
    button_labelled(&h, "Save").click();
    h.run();
    assert!(password_fields(&h).is_empty(), "the form must close on a successful save");

    let saved = vike_secrets::load_workspace_dotenv_from(Some(dir));
    let get = |name: &str| saved.get(name).map(String::as_str);
    let names: Vec<&String> = saved.keys().collect();
    assert_eq!(
        get(&format!("BINANCE_LIVE_API_KEY{ACCOUNT_SEPARATOR}ALT")),
        Some(TYPED_KEY),
        "{names:?}"
    );
    assert_eq!(
        get(&format!("BINANCE_LIVE_API_SECRET{ACCOUNT_SEPARATOR}ALT")),
        Some(TYPED_SECRET),
        "{names:?}"
    );
    // The DEFAULT account's own rows, and every row that was not named, are untouched.
    assert_eq!(get("BINANCE_LIVE_API_KEY"), Some("default-account-key-untouched"));
    assert_eq!(get("BINANCE_LIVE_API_SECRET"), Some("default-account-secret-untouched"));
    assert_eq!(get("UNRELATED_KEY"), Some("keep-me"));
    // A blank field is still "leave unchanged", labelled or not.
    assert!(!names.iter().any(|n| n.contains("API_PASSPHRASE")), "{names:?}");
    // …and the credential FILE the GUI's path names was never written: it is not a store.
    assert!(!store.exists(), "the GUI wrote a credential FILE");

    // The confirmation names the ACCOUNT — "saved credentials for binance/live" alone is ambiguous
    // the moment a second account exists.
    let text = tree_text(&h);
    assert!(text.contains("saved credentials for binance/live (account ALT)"), "{text}");
}

/// **THE TOMBSTONE'S REPLACEMENT** — the strip used to print an INSTRUCTION under a selected
/// label: *"To REMOVE account {l}, delete its `__{l}` lines from {store} by hand."* On a migrated
/// box that instruction is WRONG — `docs/decisions/0054` stopped reading `secrets.env`, and those
/// lines are `credential` ROWS in a database — so it named a file nothing loads.
///
/// This pins what stands there now on a box with no settings database: a statement that there are
/// no account ROWS, and NO destructive affordance. The rule the old text was protecting is intact
/// and is enforced one layer down instead of by having no button — `vike_secrets::edit_account`
/// REFUSES to delete a row that still owns credentials, naming them by key NAME.
#[test]
fn a_selected_label_no_longer_tells_the_operator_to_edit_a_file_by_hand() {
    let h = with_alt_preselected();
    let text = tree_text(&h);
    assert!(
        !text.contains("by hand"),
        "the hand-edit instruction is a tombstone and must not be back: {text}"
    );
    assert!(
        text.contains("account ROW"),
        "the strip must say which object it is talking about — the store's account is a ROW, and \
         the grid's account is a key-name grammar: {text}"
    );
    // ⚠ This harness's store path has no settings directory above it, so the strip lands in the
    // `Unanswerable` arm — a box with no settings database, which since the credential FILE store
    // was removed (2026-10-07) is a box with no accounts and no credentials at all. That is the
    // ANSWER and not an empty list: saying "this label has no rows" about a store that does not
    // exist would be a different claim entirely, and the strip names how to create one.
    assert!(
        text.contains("there is no settings database") && text.contains("secrets migrate --init"),
        "an unmigrated box must be told its store cannot answer the question, rather than shown \
         an empty list that reads as an answer: {text}"
    );
}

/// ⚠ **NOTHING DESTRUCTIVE IS REACHABLE WITHOUT A ROW TO ADDRESS.** With no settings database
/// there are no rows, so there is no Remove and no Deactivate — the buttons are rendered per ROW,
/// never per label, because the label is not identity and a button that guessed which row it meant
/// would be a button that deletes a stranger.
#[test]
fn no_row_means_no_remove_and_no_deactivate() {
    let h = with_alt_preselected();
    let labels = button_labels(&h);
    for destructive in ["Remove", "Confirm remove", "Deactivate", "Activate"] {
        assert!(
            !labels.iter().any(|l| l == destructive),
            "`{destructive}` must not be reachable when this store has no account rows: {labels:?}"
        );
    }
}
