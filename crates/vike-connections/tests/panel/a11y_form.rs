//! Accessibility-tree tests for the credential RAIL + DETAIL pane + masked edit form
//! ([`vike_connections::connections_ui`]).
//!
//! ⚠ The first properties below were written against the 5-column GRID this pane replaced, and
//! they survive both that replacement and decision 0156 item 6's: the ✏ still appears on exactly
//! the cells a credential can be written to — now the cells with an ACCOUNT ROW — in
//! Paper/Demo/Live order, and the form it opens is still masked and still belongs to the cell that
//! was clicked.
//!
//! 1. **"Edit fields are `egui::TextEdit::password(true)` (masked)"** — security rule 3 of the
//!    module doc, and the test only an accessibility tree can make. `.password(true)` is what
//!    makes egui file the widget as `Role::PasswordInput` instead of `Role::TextInput`, and the
//!    same call is what masks the value handed to assistive tech. Dropping it compiles, renders a
//!    normal-looking field, keeps every pure test green — and reads a live API secret aloud.
//! 2. **The ✏ opens the tier row it sits in.** `tier_row` is called three times per venue with only
//!    the tier differing; an off-by-one would open the DEMO form from the LIVE cell and save a live
//!    key into the demo account. The heading asserts which form actually opened.
//! 3. **A cell with no credential to write offers no edit affordance.** `tier_row` renders the ✏
//!    only for a cell with an account row, never for paper (the simulator reads none) and never for
//!    a tier with no row (a credential write never creates an account).
//!
//! ⚠ These tests never click **Save** — `support::NEVER_WRITTEN_SETTINGS_DIR` is the belt that says
//! so. The braces are that `connections_ui` does not resolve the store for itself: the settings
//! directory is a `vike_connections::CredentialWrite` parameter, so a harness can point it anywhere
//! and no walk is reachable from here. Nothing in this workspace may rewrite the
//! user's only copy of live venue keys (CLAUDE.md, "Credentials & the live gate"). Opening the
//! form and reading the tree is the whole interaction — no secret is typed, so none can leak.
//! (The Save arm's own end-to-end gate lives in `tests/panel/write_journal.rs`, which drives
//! it against a throwaway store and a throwaway ledger.)
//!
//! ⚠ **The test that seeds a REAL store is `a11y_secrets`'**
//! `no_stored_credential_value_reaches_the_rail_the_detail_pane_or_an_opened_editor`: every harness
//! here holds account rows with no credential at all, so there is no VALUE for a regression to
//! render. That test plants sentinel values in a scratch settings database and walks the rail, a
//! detail pane, an opened editor and a labelled account's grid asserting no secret reaches any of
//! them.

use std::collections::HashMap;

use egui::accesskit::Role;
use egui_kittest::kittest::NodeT;
use vike_connections::keys::{FormPlace, Sensitivity, form_fields, form_note};
use vike_model::AccountTier;
use vike_secrets::account_credentials::CredentialBook;

use crate::support::{edit_buttons, grids_of, harness, harness_grids, row, text_fields, tree_text};

/// dukascopy with ONE default-account row, at DEMO — its one demo account.
fn dukascopy_demo() -> egui_kittest::Harness<'static, ()> {
    let grids = grids_of(
        &["dukascopy"],
        vec![row(1, "dukascopy", AccountTier::Demo, None)],
        CredentialBook::new(),
    );
    harness_grids(grids, HashMap::new())
}

/// The role a form field must have: masked iff the schema calls it secret.
fn role_of(s: Sensitivity) -> Role {
    match s {
        Sensitivity::Secret => Role::PasswordInput,
        Sensitivity::Public => Role::TextInput,
    }
}

/// The edit affordance appears exactly on the cells with an ACCOUNT ROW.
///
/// binance's harness holds a DEMO and a LIVE row: two pencils (paper reads no credential).
/// dukascopy holds only a DEMO row, so its paper and live cells must stay dot-only. Reddens on
/// `tier_row` dropping its `TierState::NotConfigurable` guard.
#[test]
fn only_cells_with_an_account_row_offer_an_edit_button() {
    let mut generic = harness("binance");
    generic.run();
    assert_eq!(edit_buttons(&generic).len(), 2, "binance's demo and live rows");

    let mut duka = dukascopy_demo();
    duka.run();
    assert_eq!(
        edit_buttons(&duka).len(),
        1,
        "dukascopy has only a DEMO row — its paper and live cells must offer no form"
    );
}

/// SECURITY: every SECRET field of an opened credential form is a PASSWORD input in the
/// accessibility tree — not merely drawn with dots — and the form that opened belongs to the cell
/// that was clicked.
///
/// Reddens on deleting `.password(true)` from `render_edit_form`'s `TextEdit` (the role falls back
/// to `Role::TextInput`, the state in which assistive tech and any tree-reading automation are
/// entitled to speak a typed API secret out loud), and on `tier_row` passing the wrong tier.
#[test]
fn an_opened_credential_form_is_masked_and_belongs_to_the_cell_that_was_clicked() {
    let mut h = harness("binance");
    h.run();
    assert!(text_fields(&h).is_empty(), "no form is open before the edit pencil is clicked");

    // Row order is Paper, Demo, Live — click the LAST cell's ✏ and the LIVE form must open.
    let buttons = edit_buttons(&h);
    buttons.last().expect("binance offers an edit button").click();
    drop(buttons);
    h.run();
    assert!(tree_text(&h).contains("Edit binance / live"), "the LIVE row's form opened");

    let fields = text_fields(&h);
    let placeholders: Vec<String> = fields
        .iter()
        .map(|f| f.accesskit_node().placeholder().unwrap_or_default().to_string())
        .collect();
    let form = form_fields("binance", FormPlace::Tier(AccountTier::Live));
    assert_eq!(
        placeholders,
        vec![
            "API_KEY",
            "API_SECRET",
            "API_PASSPHRASE",
            // The venue-wide attribution tail — a DEPLOYMENT row, written by its name; see the role
            // assertion below, which is where this test stops being "every field is masked" and
            // becomes "every SECRET field is".
            "BINANCE_BROKER_CODE",
        ],
        "the LIVE cell's edit pencil must open the LIVE form, in `form_fields` order"
    );
    assert_eq!(placeholders, form.iter().map(|f| f.hint().to_string()).collect::<Vec<_>>());

    for (f, field) in fields.iter().zip(&form) {
        // ⚠ The rule is the schema's (`FormField::sensitivity`), not this test's. Three of these
        // four authenticate and must be `PasswordInput`. The fourth is an affiliate tag stamped on
        // outgoing orders in the clear: masking it would buy nothing and cost the operator the
        // ability to read it back.
        assert_eq!(f.accesskit_node().role(), role_of(field.sensitivity()), "{}", field.hint());
    }
    // ...and the masking really is doing work here: most of this form IS secret.
    assert_eq!(
        form.iter().filter(|f| f.sensitivity() == Sensitivity::Secret).count(),
        3,
        "a form where nothing is classified secret would pass the loop above vacuously"
    );
}

/// **THE OTHER HALF OF THE OWNER'S REPORT.** *"Editing Dukascopy credentials doesn't show them"*
/// is two complaints in one sentence: fields that were missing (closed elsewhere) and fields that
/// are DELIBERATELY empty. The second is the design and must stay — a stored plaintext is never
/// read back — but a form that silently discards what you cannot see is indistinguishable from a
/// broken one, so the panel has to SAY it where the operator is looking.
///
/// Asserted against `view::MASKED_FIELD_HINT`'s own bytes rather than a paraphrase, plus the facts
/// a reader has to come away with. ⚠ It asserts the text REACHED THE TREE.
#[test]
fn an_opened_form_says_why_it_is_empty_before_a_field_is_typed() {
    let mut h = harness("binance");
    h.run();
    let before = tree_text(&h);
    assert!(
        !before.contains(vike_connections::view::MASKED_FIELD_HINT),
        "the hint belongs to an OPEN form, not to the closed panel: {before}"
    );

    let buttons = edit_buttons(&h);
    buttons.last().expect("binance offers an edit button").click();
    drop(buttons);
    h.run();

    let text = tree_text(&h);
    assert!(
        text.contains(vike_connections::view::MASKED_FIELD_HINT),
        "the masked-field hint must reach the screen with the form: {text}"
    );
    // The facts, each stated in the operator's own terms.
    for phrase in ["never read back", "starts EMPTY", "REPLACE", "KEEP"] {
        assert!(text.contains(phrase), "the hint must still say `{phrase}`: {text}");
    }
    // ...and every SECRET field is still masked. The wording change must never be paid for with
    // the masking, which is what "just show me the value" would actually cost.
    let form = form_fields("binance", FormPlace::Tier(AccountTier::Live));
    let secrets: Vec<Role> = text_fields(&h)
        .iter()
        .zip(&form)
        .filter(|(_, field)| field.sensitivity() == Sensitivity::Secret)
        .map(|(f, _)| f.accesskit_node().role())
        .collect();
    assert_eq!(secrets, vec![Role::PasswordInput; 3], "binance's three API fields stay masked");
}

/// **Dukascopy's DEMO form writes ONE account's login pair, and names the server SETTING beside
/// it.** Its two demo accounts are two labelled rows now (decision 0156 item 6), so the form carries
/// one account's fields; the JForex server is the demo tier's `venue.dukascopy.demo.server` setting,
/// so the form writes none and its note says where the server is set — with the one condition the
/// mount puts on the value (`vike_dukascopy::exec`'s `DukascopyExec::spawn_with_program` keeps it
/// only when it ends in `.jnlp`).
#[test]
fn the_dukascopy_demo_form_writes_one_login_pair_and_names_the_server_setting() {
    let mut duka = dukascopy_demo();
    duka.run();
    let buttons = edit_buttons(&duka);
    assert_eq!(buttons.len(), 1, "dukascopy offers exactly one editable cell (its DEMO row)");
    buttons[0].click();
    drop(buttons);
    duka.run();

    let fields = text_fields(&duka);
    let placeholders: Vec<String> = fields
        .iter()
        .map(|f| f.accesskit_node().placeholder().unwrap_or_default().to_string())
        .collect();
    assert_eq!(placeholders, vec!["LOGIN", "PASSWORD"], "one demo account, WHOLE");
    // ⚠ NOT "masked like every other one" — that is what this panel used to do and is exactly the
    // half of the owner's report that was a design flaw rather than a missing field. A login NAMES a
    // thing; masking it is how a value an operator set six months ago becomes unknowable from
    // inside the app that wrote it. The password beside it is still masked.
    let form = form_fields("dukascopy", FormPlace::Tier(AccountTier::Demo));
    let roles: Vec<Role> = fields.iter().map(|f| f.accesskit_node().role()).collect();
    let want: Vec<Role> = form.iter().map(|f| role_of(f.sensitivity())).collect();
    assert_eq!(roles, want);
    assert_eq!(roles, vec![Role::TextInput, Role::PasswordInput], "the PASSWORD is still masked");
    drop(fields);

    let text = tree_text(&duka);
    let note =
        form_note("dukascopy", FormPlace::Tier(AccountTier::Demo)).expect("this form has one");
    assert!(
        note.contains(".jnlp") && note.contains("venue.dukascopy.demo.server"),
        "the note itself: {note}"
    );
    assert!(text.contains(note), "the condition must reach the screen with the form: {text}");

    // ...and no other venue's form claims a condition it does not have.
    let mut bin = harness("binance");
    bin.run();
    let buttons = edit_buttons(&bin);
    buttons.last().expect("binance offers an edit button").click();
    drop(buttons);
    bin.run();
    assert!(
        !tree_text(&bin).contains("No live smoke"),
        "the note is per-form — binance's form must carry none"
    );
}
