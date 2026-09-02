//! **The Connections panel's account dimension**, driven through the real widget
//! (`vike_connections::connections_ui`) and asserted over the accessibility tree.
//!
//! `crates/vike-connections/tests/account_grids.rs` gates the pure enumeration; this file gates the
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
//! Same reason as `crates/vike-connections/tests/account_status.rs`: the settings-registry scanner
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
    connections_ui, AccountGrids, ConnectionState, CredentialWrite, VenueCredStatus,
};
use vike_model::account_keys::{AccountLabel, ACCOUNT_SEPARATOR};
use vike_model::scratch::ScratchDir;

/// The glyph `status_cell` labels its edit button with.
const PENCIL: &str = "\u{270E}";
/// The glyph the SELECTED account chip is marked with.
const DOT: &str = "\u{25CF}";

/// A store path these tests never write, and which no walk produced — the same belt
/// `connections_a11y.rs` wears, for the same reason.
const NEVER_WRITTEN_STORE: &str = "account-editor-tests-never-save-to-this.env";

/// The two values typed into a labelled LIVE form. No six-character run of either appears in a
/// venue name, a tier, a key name or a label.
const TYPED_KEY: &str = "zqxjvw7413mfbphgnd8256wu";
const TYPED_SECRET: &str = "pfgzmwqx9042hvbjntdu6531";

fn label(text: &str) -> AccountLabel {
    AccountLabel::parse(text).expect("a legal label")
}

fn binance_row(configured: bool) -> VenueCredStatus {
    VenueCredStatus { venue: "binance".to_string(), sim: false, demo: false, live: configured }
}

/// ONE venue's row, so a count of buttons is a count of THAT venue's affordances.
fn harness(grids: AccountGrids, creds: CredentialWrite<'static>) -> Harness<'static, ()> {
    let live: HashMap<String, ConnectionState> = HashMap::new();
    Harness::builder().with_size(egui::vec2(1200.0, 900.0)).build_ui(move |ui| {
        connections_ui(ui, &grids, &live, creds, None);
    })
}

/// The harness above **driven the way the production caller drives a preselection**: the pending
/// account is `take`n, so it is offered on the first frame and never again.
///
/// ⚠ The `take` is the point, and it belongs to the caller:
/// `crates/vike-app-core/src/tool_views/connections.rs`'s `connections_tool_content` takes
/// `ToolView::connections_account` rather than reading it. Reproducing the caller's
/// shape here is what lets the ONE-SHOT property be tested at all: `connections_ui` itself honours
/// whatever it is handed every frame it is handed one (see its doc), so a harness that re-offered
/// the same `Some` forever would be testing a caller nobody has.
fn harness_preselect(
    grids: AccountGrids,
    creds: CredentialWrite<'static>,
    preselect: AccountLabel,
) -> Harness<'static, ()> {
    let live: HashMap<String, ConnectionState> = HashMap::new();
    let mut pending = Some(preselect);
    Harness::builder().with_size(egui::vec2(1200.0, 900.0)).build_ui(move |ui| {
        connections_ui(ui, &grids, &live, creds, pending.take());
    })
}

fn dry_creds() -> CredentialWrite<'static> {
    CredentialWrite { store: Path::new(NEVER_WRITTEN_STORE), journal: None, now_ms: 0 }
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

fn nodes<'t, 'h>(h: &'t Harness<'h, ()>, pred: impl Fn(&Node<'t>) -> bool) -> Vec<Node<'t>> {
    h.root().children_recursive().filter(|n| pred(n)).collect()
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

fn password_fields<'t, 'h>(h: &'t Harness<'h, ()>) -> Vec<Node<'t>> {
    nodes(h, |n| n.accesskit_node().role() == Role::PasswordInput)
}

fn text_inputs<'t, 'h>(h: &'t Harness<'h, ()>) -> Vec<Node<'t>> {
    nodes(h, |n| n.accesskit_node().role() == Role::TextInput)
}

fn placeholders(h: &Harness<'_, ()>) -> Vec<String> {
    password_fields(h)
        .iter()
        .map(|f| f.accesskit_node().placeholder().unwrap_or_default().to_string())
        .collect()
}

/// Everything the tree says, one node per line — label and value both, because egui files a
/// `Role::Label`'s text under `value` rather than `label`.
fn tree_text(h: &Harness<'_, ()>) -> String {
    h.root()
        .children_recursive()
        .map(|n| {
            let a = n.accesskit_node();
            let mut s = String::new();
            if let Some(l) = a.label() {
                s.push_str(&l);
                s.push(' ');
            }
            if let Some(v) = a.value() {
                s.push_str(&v);
            }
            s
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Open a venue's LIVE credential form — the LAST ✎, row order being Sim, Demo, Live.
fn open_live_form<'h>(h: &mut Harness<'h, ()>) {
    let pencils = nodes(h, |n| {
        let a = n.accesskit_node();
        a.role() == Role::Button && a.label().as_deref() == Some(PENCIL)
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
        vec!["Add account".to_string(), PENCIL.to_string(), PENCIL.to_string(), PENCIL.to_string()],
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
        vec!["BINANCE_LIVE_API_KEY", "BINANCE_LIVE_API_SECRET", "BINANCE_LIVE_API_PASSPHRASE"],
        "the default account's form writes the key names it always wrote"
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
        ],
        "every field of the ALT form writes an ALT key"
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
        ],
        "the preselection reached the key composition, not just the chip's paint"
    );
}

/// ⚠ **A preselection is a ONE-SHOT, so it cannot make the chip strip inert.**
///
/// The panel's selection lives in the widget's egui temp memory, which is re-read every frame — so
/// a caller that kept handing the same `Some` down would re-select that account after every click,
/// and the chips would land, repaint, and revert with nothing on screen explaining why. The
/// production caller (`vike_app_core::tool_views::connections_tool_content`) answers that by
/// `take`ing `ToolView::connections_account`, and [`harness_preselect`] reproduces exactly that.
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
/// The message is `vike_model::account_keys::AccountKeyError`'s `Display` rather than a second
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

/// **There is no removal affordance, and there is an instruction instead.**
///
/// Nothing in this workspace deletes, moves, truncates or wholesale-rewrites the credential store —
/// it is the operator's only copy of live venue keys. A Remove button could only be implemented by
/// deleting lines from that file, so the panel names the file and the line shape and stops.
/// Reddens the moment such a button is added.
#[test]
fn the_account_panel_offers_no_removal_affordance() {
    let mut h = with_alt();
    h.run();
    click(&mut h, "ALT");

    let labels = button_labels(&h);
    for forbidden in ["Delete", "Remove", "Delete account", "Remove account", "Forget"] {
        assert!(
            !labels.iter().any(|l| l == forbidden),
            "a {forbidden:?} button would have to delete lines from the credential store: {labels:?}"
        );
    }
    let text = tree_text(&h);
    assert!(text.contains("To REMOVE account ALT"), "the instruction must be on screen: {text}");
    assert!(
        text.contains(&format!("{ACCOUNT_SEPARATOR}ALT")),
        "…and must name the line shape to delete: {text}"
    );
    assert!(text.contains("Nothing in vike deletes a credential"), "{text}");
}

/// **THE write gate: a labelled Save writes ONLY that account's keys, and leaves the default
/// account's line byte-identical.**
///
/// This is the whole of the "the machinery exists — do not rebuild it" claim, proven against a real
/// store: `vike_secrets::save_credentials` is key-name agnostic, so the account dimension is
/// carried entirely by the names `account_fields` composes, and the byte-preserving upsert does the
/// rest.
#[test]
fn a_labelled_save_writes_only_that_accounts_keys_and_preserves_the_default_accounts() {
    let root =
        ScratchDir::create_in(&std::env::temp_dir(), "vike-account-editor").expect("scratch root");
    let store = root.path().join("secrets.env");
    // ⚠ The store is SEEDED through `save_credentials` itself rather than a raw file write: it is
    // the sanctioned writer, so the fixture cannot encode a store shape the app could not have
    // produced, and the pre-existing default-account lines below are ones this program really does
    // write.
    //
    // (Historical note worth keeping, because it is the kind of thing that gets "cleaned up" back:
    // this was first written as an `fs::write` and `crates/vike-ops/tests/journal_scratch_gate.rs`
    // refused it — that rule flags a file which mints a path under `env::temp_dir()` and creates
    // something there without a self-deleting handle. `ScratchDir`'s `Drop` was always real; the
    // gate's needle at the time could not see it, and now names `ScratchDir::create_in` outright.
    // So this write is no longer what keeps the gate green — the reason above is why it stays.)
    //
    // What is therefore NOT asserted below: comment and blank-line preservation. That property
    // belongs to the transform and is gated where it lives, by `vike_secrets::env_write`'s
    // `every_untouched_line_is_byte_identical_and_in_order`. What IS asserted is this file's own
    // question — that a LABELLED save leaves the DEFAULT account's keys, and an unrelated key,
    // exactly where they were.
    vike_connections::env_write::save_credentials(
        &store,
        &[
            ("BINANCE_LIVE_API_KEY".to_string(), "default-account-key-untouched".to_string()),
            ("BINANCE_LIVE_API_SECRET".to_string(), "default-account-secret-untouched".to_string()),
            ("UNRELATED_KEY".to_string(), "keep-me".to_string()),
        ],
    )
    .expect("seed the store");

    let creds = CredentialWrite { store: &store, journal: None, now_ms: 0 };
    let live: HashMap<String, ConnectionState> = HashMap::new();
    let grids =
        AccountGrids::new(vec![binance_row(true)], vec![(label("ALT"), vec![binance_row(false)])]);
    let mut h = Harness::builder().with_size(egui::vec2(1200.0, 900.0)).build_ui(move |ui| {
        connections_ui(ui, &grids, &live, creds, None);
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

    let saved = std::fs::read_to_string(&store).expect("the store was written");
    assert!(
        saved.contains(&format!("BINANCE_LIVE_API_KEY{ACCOUNT_SEPARATOR}ALT={TYPED_KEY}")),
        "{saved}"
    );
    assert!(
        saved.contains(&format!("BINANCE_LIVE_API_SECRET{ACCOUNT_SEPARATOR}ALT={TYPED_SECRET}")),
        "{saved}"
    );
    // The DEFAULT account's own lines, and every line that was not named, are byte-identical.
    assert!(saved.contains("BINANCE_LIVE_API_KEY=default-account-key-untouched"), "{saved}");
    assert!(saved.contains("BINANCE_LIVE_API_SECRET=default-account-secret-untouched"), "{saved}");
    assert!(saved.contains("UNRELATED_KEY=keep-me"), "{saved}");
    // A blank field is still "leave unchanged", labelled or not.
    assert!(!saved.contains("API_PASSPHRASE"), "{saved}");

    // The confirmation names the ACCOUNT — "saved credentials for binance/live" alone is ambiguous
    // the moment a second account exists.
    let text = tree_text(&h);
    assert!(text.contains("saved credentials for binance/live (account ALT)"), "{text}");
}
