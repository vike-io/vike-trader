//! **The Gateway panel's account dimension**, driven through the real widget
//! (`vike_gateway_panel::gateway_ui`) and asserted over the accessibility tree.
//!
//! `crates/vike-gateway-panel/tests/key_shapes/account_grids.rs` gates the pure enumeration; this file
//! gates the SCREEN and the WRITE — which chips exist, which account a form belongs to, what the
//! form writes, and what an invalid label does before anything reaches disk.
//!
//! # The contract this change is held to
//!
//! **A label is an account ROW's, and a form writes the row it was opened for** (decision 0156
//! item 6). [`a_labelled_save_writes_only_that_accounts_fields_and_preserves_the_default_accounts`]
//! is the end-to-end proof against a real store: a Save from a labelled account's form lands on
//! THAT row, by field, and the default account's row and an unrelated deployment row are untouched.
//!
//! ⚠ Where a test clicks **Save**, it writes to a `vike_paths::scratch::ScratchDir`, never to a
//! resolved store — `gateway_ui` takes its store as a `CredentialWrite` parameter, which is
//! what makes that safe (see `write_journal.rs`'s header).

use std::path::Path;

use egui::accesskit::Role;
use egui_kittest::kittest::NodeT;
use egui_kittest::{Harness, Node};
use vike_gateway_panel::{AccountGrids, CredentialWrite};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::CredentialOwner;
use vike_secrets::account_credentials::CredentialBook;

use crate::support::{
    NEVER_WRITTEN_SETTINGS_DIR, PanelHarness, TYPED_KEY, TYPED_SECRET, create_account, dry_creds,
    grids_of, nodes, password_fields, pencil, row, scratch_store, seed_fields, store_grids,
    stored_field, test_proc, tree_text,
};

/// The glyph the SELECTED account chip is marked with.
const DOT: &str = "\u{25CF}";

/// ONE venue's rows, so a count of buttons is a count of THAT venue's affordances.
fn harness(grids: AccountGrids, creds: CredentialWrite<'static>) -> Harness<'static, ()> {
    PanelHarness { size: egui::vec2(1200.0, 900.0), creds, run: false, ..PanelHarness::new(grids) }
        .build()
}

/// The harness above **driven the way the production caller drives a preselection**: the pending
/// account is `take`n, so it is offered on the first frame and never again.
///
/// ⚠ The `take` is the point, and it belongs to the caller:
/// `crates/vike-app-core/src/ui/tool_views/data.rs`'s `data_body` (its `DataDest::Gateway`
/// arm) takes `ToolView::gateway_account` rather than reading it. Reproducing the caller's
/// shape here is what lets the ONE-SHOT property be tested at all: `gateway_ui` itself honours
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

/// binance's DEFAULT account with a DEMO row (1) and a LIVE row (2) — the shape a box whose rows
/// carry no label has.
fn default_only_grids() -> AccountGrids {
    grids_of(
        &["binance"],
        vec![
            row(1, "binance", AccountTier::Demo, None),
            row(2, "binance", AccountTier::Live, None),
        ],
        CredentialBook::new(),
    )
}

/// The same plus an `ALT` account with its own DEMO (3) and LIVE (4) rows.
fn with_alt_grids() -> AccountGrids {
    grids_of(
        &["binance"],
        vec![
            row(1, "binance", AccountTier::Demo, None),
            row(2, "binance", AccountTier::Live, None),
            row(3, "binance", AccountTier::Demo, Some("ALT")),
            row(4, "binance", AccountTier::Live, Some("ALT")),
        ],
        CredentialBook::new(),
    )
}

fn default_only() -> Harness<'static, ()> {
    harness(default_only_grids(), dry_creds())
}

fn with_alt() -> Harness<'static, ()> {
    harness(with_alt_grids(), dry_creds())
}

/// The same panel, opened with `ALT` already selected — the QA capture arm's configuration.
fn with_alt_preselected() -> Harness<'static, ()> {
    harness_preselect(with_alt_grids(), dry_creds(), label("ALT"))
}

fn label(text: &str) -> AccountLabel {
    AccountLabel::parse(text).expect("a legal label")
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

/// Every OPEN form field's placeholder — the field it writes — in field order.
///
/// ⚠ **Both roles, not just `PasswordInput`.** A field that NAMES something (an attribution code,
/// a login) is an ordinary `Role::TextInput` so an operator can read it back, and a helper that
/// looked only at password inputs would go quietly blind to exactly those fields.
fn form_fields<'t, 'h>(h: &'t Harness<'h, ()>) -> Vec<Node<'t>> {
    nodes(h, |n| matches!(n.accesskit_node().role(), Role::PasswordInput | Role::TextInput))
}

fn placeholders(h: &Harness<'_, ()>) -> Vec<String> {
    form_fields(h)
        .iter()
        .map(|f| f.accesskit_node().placeholder().unwrap_or_default().to_string())
        .collect()
}

/// Open the venue's LIVE credential form — the LAST edit pencil, row order being Paper, Demo,
/// Live — asserting `rows` pencils are on screen (one per account row of the selected account).
fn open_live_form(h: &mut Harness<'_, ()>, rows: usize) {
    let pencils = nodes(h, |n| {
        let a = n.accesskit_node();
        a.role() == Role::Button && a.label().as_deref() == Some(pencil().as_str())
    });
    assert_eq!(pencils.len(), rows, "one pencil per account row of the selected account");
    pencils.last().expect("an edit button").click();
    drop(pencils);
    h.run();
}

/// Click a button, deliver the click.
fn click(h: &mut Harness<'_, ()>, want: &str) {
    {
        // Scoped so the tree borrow ends before `run` needs the harness mutably.
        button_labelled(h, want).click();
    }
    h.run();
}

/// **THE single-account panel.** With no labelled row the whole button set is one pencil per
/// account row plus a single `Add account`, the default account is the one selected, no form is
/// open, and the LIVE form's placeholders are the FIELDS the schema row names plus the venue-wide
/// attribution code.
///
/// Reddens on a chip appearing for an account no row carries, and on the selector defaulting to
/// anything but the default account.
#[test]
fn a_store_with_no_labelled_account_renders_the_panel_that_shipped() {
    let mut h = default_only();
    h.run();

    let mut labels = button_labels(&h);
    labels.sort();
    assert_eq!(
        labels,
        vec!["Add account".to_string(), pencil(), pencil()],
        "one edit affordance per account row (demo and live), and one way to name an account"
    );
    let text = tree_text(&h);
    assert!(text.contains(&format!("{DOT} default")), "the default account is selected: {text}");
    assert!(password_fields(&h).is_empty(), "no credential form is open");
    assert!(text_inputs(&h).is_empty(), "the Add-account field is closed until it is asked for");

    open_live_form(&mut h, 2);
    assert_eq!(
        placeholders(&h),
        vec![
            "API_KEY",
            "API_SECRET",
            "API_PASSPHRASE",
            // ⚠ The VENUE-WIDE attribution tail, appended to every mechanised venue's LIVE form. It
            // is a DEPLOYMENT row read by its name, never the account's — `vike_gateway_panel::keys`'
            // `attribution_fields` argues why.
            "BINANCE_BROKER_CODE",
        ],
        "the default account's form writes the schema row's fields, plus the venue-wide code"
    );
}

/// A labelled account is one click away, and ITS form is its OWN row's.
///
/// Reddens on the form ignoring the selection (the heading and the row caption keep naming the
/// default account), and on the chip strip rendering the selected account as a button (which
/// would make "which account am I editing" unanswerable from the tree).
#[test]
fn a_labelled_account_is_selectable_and_its_form_is_that_accounts_row() {
    let mut h = with_alt();
    h.run();
    assert!(tree_text(&h).contains(&format!("{DOT} default")), "default starts selected");
    assert!(tree_text(&h).contains("account 2"), "the default account's LIVE row is row 2");

    click(&mut h, "ALT");
    let text = tree_text(&h);
    assert!(text.contains(&format!("{DOT} ALT")), "ALT is now the selected account: {text}");
    assert!(
        button_labels(&h).iter().any(|l| l == "default"),
        "…and the default account became the clickable chip"
    );
    assert!(text.contains("account 4") && !text.contains("account 2"), "ALT's rows: {text}");

    open_live_form(&mut h, 2);
    let text = tree_text(&h);
    assert!(text.contains("Edit binance / live · account ALT"), "the form names ALT: {text}");
    assert_eq!(
        placeholders(&h),
        vec!["API_KEY", "API_SECRET", "API_PASSPHRASE", "BINANCE_BROKER_CODE"],
        "an account's FIELDS carry no label — the row the form writes is what differs"
    );
}

/// **The QA capture arm's seam: a PRESELECTED account is selected on the first frame, unclicked.**
///
/// `scripts/qa_shots.sh`'s `05b-gateway-account` capture exists because two things this panel
/// renders only for a labelled account — the chip strip and the account-row block — are reachable
/// by no headless layout test and, before the preselect parameter, by no capture either.
///
/// ⚠ It asserts the ROW as well as the chip, and that is the "right answer for the wrong reason"
/// guard. A preselect that moved the chip but not `EditState::account` would draw a convincing
/// capture of the ALT account over the DEFAULT account's rows — the cells' `account N` captions are
/// what tell them apart.
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
    assert!(
        text.contains("account 3") && text.contains("account 4") && !text.contains("account 2"),
        "the preselection reached the cells, not just the chip's paint: {text}"
    );

    open_live_form(&mut h, 2);
    assert!(
        tree_text(&h).contains("Edit binance / live · account ALT"),
        "…and the form: {}",
        tree_text(&h)
    );
}

/// ⚠ **A preselection is a ONE-SHOT, so it cannot make the chip strip inert.**
///
/// The panel's selection lives in the widget's egui temp memory, which is re-read every frame — so
/// a caller that kept handing the same `Some` down would re-select that account after every click,
/// and the chips would land, repaint, and revert with nothing on screen explaining why. The
/// production caller (`vike_app_core::ui::tool_views::data_tool_content`'s `data_body`, on
/// `DataDest::Gateway`) answers that by `take`ing `ToolView::gateway_account`, and
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
/// across a switch would leave a form for one account's row under another account's chip — a live
/// key the operator believes is going to the account on screen. Reddens on
/// `EditState::select_account` dropping its `close()`.
#[test]
fn switching_account_closes_an_open_credential_form() {
    let mut h = with_alt();
    h.run();
    open_live_form(&mut h, 2);
    assert_eq!(password_fields(&h).len(), 3, "the default account's LIVE form is open");

    click(&mut h, "ALT");
    assert!(
        password_fields(&h).is_empty(),
        "the form must close on an account switch — it is still open, so its buffers now sit \
         under an account they were not typed for"
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
/// The message is `vike_model::accounts::account_keys::AccountKeyError`'s `Display` rather than a
/// second wording maintained here, so a rule change cannot leave the screen explaining the old
/// rule. Reddens on `EditState::select_typed_account` accepting an unvalidated string.
#[test]
fn an_invalid_account_label_is_refused_with_the_validators_own_reason() {
    // Lowercase: refused rather than repaired, deliberately — `AccountLabel::parse` says why.
    let text = offer_label("alt");
    assert!(text.contains("is neither"), "the char refusal must name the offending byte: {text}");
    assert!(text.contains(&format!("{DOT} default")), "…and no account was selected: {text}");
    assert!(!text.contains(&format!("{DOT} alt")), "the refused label must not become a chip");

    // The reserved spelling for the unlabelled account.
    let text = offer_label("DEFAULT");
    assert!(text.contains("reserved"), "the reserved-label refusal must be shown: {text}");

    // Longer than the grammar's ceiling.
    let text = offer_label("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
    assert!(text.contains("at most"), "the length refusal must be shown: {text}");

    // Nothing typed at all.
    assert!(
        !Path::new(NEVER_WRITTEN_SETTINGS_DIR).exists(),
        "not one of these refusals may have touched a store"
    );
}

/// A VALID label is accepted, selected, marked as carried by no row yet — and writes NOTHING.
///
/// An account is a ROW the operator creates with its venue and tier; selecting a label creates
/// none, and a credential Save never creates one either (decision 0156 item 6).
#[test]
fn a_valid_label_is_accepted_selects_the_account_and_writes_nothing() {
    let text = offer_label("HEDGE");
    assert!(text.contains(&format!("{DOT} HEDGE (new)")), "the new account is selected: {text}");
    assert!(
        text.contains("no account row carries the label HEDGE yet"),
        "a label no row carries must say so rather than showing bare cells: {text}"
    );
    assert!(
        text.contains("vike-cli secrets account add --venue binance --tier demo --label HEDGE"),
        "…and each cell names the command that creates its row: {text}"
    );
    assert!(!Path::new(NEVER_WRITTEN_SETTINGS_DIR).exists(), "naming an account writes nothing");
}

/// **The panel deletes no CREDENTIAL, and removal is a ROW act behind a typed confirm.**
///
/// `docs/decisions/0065` is the record. The rule is enforced one layer down rather than by having
/// no button: nothing in this workspace deletes, moves, truncates or wholesale-rewrites the
/// credential store, and `vike_secrets::edit_account` REFUSES to delete an account row that still
/// owns credentials. So what this test pins is the VOCABULARY — no affordance here claims to delete
/// a credential or an "account" as a whole — and the absence of any row act on a store that has no
/// rows.
#[test]
fn the_panel_offers_no_credential_deleting_affordance() {
    let mut h = with_alt();
    h.run();
    click(&mut h, "ALT");

    let labels = button_labels(&h);
    // ⚠ "Remove" is deliberately NOT in this list: a per-ROW Remove is a legitimate affordance (it
    // deletes an `account` row, refuses while that row owns credentials, and needs a typed
    // confirm), while every word below claims to delete the operator's KEYS.
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
        "the hand-edit instruction is a tombstone and must not come back: {text}"
    );
    assert!(
        text.contains("nothing here erases a credential"),
        "the panel must still say the rule out loud: {text}"
    );
}

/// **THE write gate: a labelled Save writes ONLY that account row's fields, and leaves the default
/// account's row — and a deployment row — exactly where they were.**
///
/// Against a real store: the default account's LIVE row holds a key and a secret, the ALT
/// account's LIVE row holds nothing, and an unrelated DEPLOYMENT row sits beside them. A Save
/// from ALT's form lands on ALT's row, by field, through `vike_secrets::save_owned_credentials`.
#[test]
fn a_labelled_save_writes_only_that_accounts_fields_and_preserves_the_default_accounts() {
    let root = scratch_store("vike-account-editor");
    let default_live = create_account(root.path(), "binance", AccountTier::Live, None);
    let alt_live = create_account(root.path(), "binance", AccountTier::Live, Some("ALT"));
    seed_fields(
        root.path(),
        &CredentialOwner::Account(default_live),
        &[("API_KEY", "default-account-key-untouched"), ("API_SECRET", "default-secret-untouched")],
    );
    vike_secrets::save_credentials_to_store_unjournalled(
        root.path(),
        vike_secrets::Table::Credential,
        &[("UNRELATED_KEY".to_string(), "keep-me".to_string())],
        vike_secrets::UpsertIntent::Add,
    )
    .expect("seed a deployment row");

    let creds =
        CredentialWrite { settings_dir: root.path(), journal: None, proc: test_proc(), now_ms: 0 };
    let grids = store_grids(root.path());
    let mut h = Harness::builder().with_size(egui::vec2(1200.0, 900.0)).build_ui(move |ui| {
        if vike_ui_theme::harness::type_ready(ui.ctx()) {
            vike_gateway_panel::gateway_ui(ui, &grids, &Default::default(), creds, None);
        }
    });
    h.run();

    button_labelled(&h, "ALT").click();
    h.run();
    open_live_form(&mut h, 1);

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

    assert_eq!(stored_field(root.path(), alt_live, "API_KEY").as_deref(), Some(TYPED_KEY));
    assert_eq!(stored_field(root.path(), alt_live, "API_SECRET").as_deref(), Some(TYPED_SECRET));
    // A blank field is still "leave unchanged".
    assert_eq!(stored_field(root.path(), alt_live, "API_PASSPHRASE"), None);
    // The DEFAULT account's own row, and the row that was not named, are untouched.
    assert_eq!(
        stored_field(root.path(), default_live, "API_KEY").as_deref(),
        Some("default-account-key-untouched")
    );
    assert_eq!(
        stored_field(root.path(), default_live, "API_SECRET").as_deref(),
        Some("default-secret-untouched")
    );
    let names = crate::support::store_at(root.path())
        .read_all()
        .expect("the credential store reads")
        .secrets
        .into_map();
    assert_eq!(names.get("UNRELATED_KEY").map(String::as_str), Some("keep-me"));

    // The confirmation names the ACCOUNT — "saved credentials for binance/live" alone is ambiguous
    // the moment a second account exists.
    let text = tree_text(&h);
    assert!(text.contains("saved credentials for binance/live (account ALT)"), "{text}");
}

/// **No hand-edit instruction under a selected label** — the strip names the account ROW instead.
///
/// This pins what stands there on a box with no settings database: a statement that there are
/// no account ROWS, and NO destructive affordance.
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
        "the strip must say which object it is talking about — the store's account is a ROW: \
         {text}"
    );
    // ⚠ This harness's settings directory holds no database, so the strip lands in the
    // `Unanswerable` arm — a box with no settings database is a box with no accounts and no
    // credentials at all. That is the ANSWER and not an empty list, and the strip names how to
    // create one.
    assert!(
        text.contains("there is no settings database") && text.contains("secrets init"),
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
