//! Accessibility-tree tests for the credential RAIL + DETAIL pane + masked edit form
//! ([`vike_connections::connections_ui`]).
//!
//! ⚠ The first three properties below were written against the 5-column GRID this pane replaced,
//! and they survive that replacement UNCHANGED — which is the point of keeping them: the ✏ still
//! appears on exactly the tiers a venue can store keys for, in Sim/Demo/Live order, and the form
//! it opens is still masked and still belongs to the cell that was clicked. The rewrite moved the
//! layout; it may not have moved any of that. The properties AFTER them are the ones the detail
//! pane adds.
//!
//! `view.rs`'s own tests all call the pure `edit_fields` helper directly; the RENDER was untested,
//! which left three of the module's stated rules asserted nowhere:
//!
//! 1. **"Edit fields are `egui::TextEdit::password(true)` (masked)"** — security rule 3 of the
//!    module doc, and the test only an accessibility tree can make. `.password(true)` is what
//!    makes egui file the widget as `Role::PasswordInput` instead of `Role::TextInput`, and the
//!    same call is what masks the value handed to assistive tech. Dropping it compiles, renders a
//!    normal-looking field, keeps every pure test green — and reads a live API secret aloud.
//! 2. **The ✏ opens the tier row it sits in.** `tier_row` is called three times per venue with only
//!    the tier string differing; an off-by-one would open the SIM form from the LIVE cell and save
//!    a live key into a sim var. The placeholders assert which form actually opened.
//! 3. **A tier with no credentials to write offers no edit affordance.** `tier_row` renders the
//!    ✏ only when `edit_fields(venue, tier)` is non-empty. The pure tests pin what `edit_fields`
//!    RETURNS; nothing pinned that the view still consults it, so a `tier_row` that stopped
//!    asking would open a form writing `DUKASCOPY_SIM_*` vars no bridge reads.
//!
//! ⚠ These tests never click **Save** — `support::NEVER_WRITTEN_STORE` is the belt that says so. The
//! braces are that `connections_ui` no longer resolves the store for itself: the path is now a
//! `vike_connections::CredentialWrite` parameter, so a harness can point it anywhere and the real
//! `workspace_dotenv_path()` is unreachable from here. Nothing in this workspace may rewrite the
//! user's only copy of live venue keys (CLAUDE.md, "Credentials & the live gate"). Opening the
//! form and reading the tree is the whole interaction — no secret is typed, so none can leak.
//! (The Save arm's own end-to-end gate lives in `tests/panel/write_journal.rs`, which drives
//! it against a throwaway store and a throwaway ledger.)
//!
//! ⚠ **One test seeds a REAL store, and it is the one that matters most.** Every harness above
//! builds an all-`false` grid, so until
//! `a11y_secrets`' `no_stored_credential_value_reaches_the_rail_the_detail_pane_or_an_opened_editor` existed
//! there was no VALUE anywhere on the box for a regression to render — a pane that started
//! pre-filling the edit buffers from the store, the exact thing security rule 1 forbids, passed
//! every assertion in this file. That test plants a sentinel value in a scratch `secrets.env`,
//! derives the grid from the same map the binary would, and walks the rail, a detail pane, an
//! opened editor and a labelled account's grid asserting the sentinel reaches none of them. It
//! still never clicks Save.

use egui::accesskit::Role;
use egui_kittest::kittest::NodeT;

use crate::support::{edit_buttons, harness, text_fields, tree_text};

/// The edit affordance appears exactly on the tiers a venue can actually store keys for.
///
/// binance has all three (generic `_API_KEY`/`_API_SECRET`/`_API_PASSPHRASE` at Sim/Demo/Live);
/// dukascopy has ONLY Demo — no Sim or Live tier exists for it, so those two cells must stay
/// dot-only. Reddens on `tier_row` dropping its `TierState::NotConfigurable` guard, which the
/// pure `dukascopy_sim_and_live_have_no_edit_fields` test cannot see.
#[test]
fn only_tiers_with_credentials_to_write_offer_an_edit_button() {
    let mut generic = harness("binance");
    generic.run();
    assert_eq!(edit_buttons(&generic).len(), 3, "binance stores keys at all three tiers");

    let mut duka = harness("dukascopy");
    duka.run();
    assert_eq!(
        edit_buttons(&duka).len(),
        1,
        "dukascopy has only a DEMO tier — its Sim and Live cells must offer no form"
    );
}

/// SECURITY: every field of an opened credential form is a PASSWORD input in the accessibility
/// tree — not merely drawn with dots — and the form that opened belongs to the cell that was
/// clicked.
///
/// Reddens on deleting `.password(true)` from `render_edit_form`'s `TextEdit` (the role falls back
/// to `Role::TextInput`, the state in which assistive tech and any tree-reading automation are
/// entitled to speak a typed API secret out loud), and on `tier_row` passing the wrong tier.
#[test]
fn an_opened_credential_form_is_masked_and_belongs_to_the_cell_that_was_clicked() {
    let mut h = harness("binance");
    h.run();
    assert!(text_fields(&h).is_empty(), "no form is open before the edit pencil is clicked");

    // Row order is Sim, Demo, Live — click the LAST cell's ✏ and the LIVE form must open.
    let buttons = edit_buttons(&h);
    buttons.last().expect("binance offers an edit button").click();
    drop(buttons);
    h.run();

    let fields = text_fields(&h);
    let placeholders: Vec<String> = fields
        .iter()
        .map(|f| f.accesskit_node().placeholder().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        placeholders,
        vec![
            "BINANCE_LIVE_API_KEY",
            "BINANCE_LIVE_API_SECRET",
            "BINANCE_LIVE_API_PASSPHRASE",
            // The venue-wide attribution tail — see the role assertion below, which is where this
            // test stops being "every field is masked" and becomes "every SECRET field is".
            "BINANCE_BROKER_CODE",
        ],
        "the LIVE cell's edit pencil must open the LIVE form, in `edit_fields` order"
    );

    for (f, key) in fields.iter().zip(&placeholders) {
        // ⚠ The rule is `key_sensitivity`'s, not this test's. Three of these four authenticate and
        // must be `PasswordInput` — the state assistive tech and any tree-reading automation are
        // entitled to speak aloud. The fourth is an affiliate tag stamped on outgoing orders in the
        // clear: masking it would buy nothing and cost the operator the ability to read it back,
        // which is the failure the owner reported about this panel.
        let want = match vike_connections::keys::key_sensitivity(key) {
            vike_connections::keys::Sensitivity::Secret => Role::PasswordInput,
            vike_connections::keys::Sensitivity::Public => Role::TextInput,
        };
        assert_eq!(f.accesskit_node().role(), want, "{key}");
    }
    // ...and the masking really is doing work here: most of this form IS secret.
    assert_eq!(
        placeholders
            .iter()
            .filter(|k| vike_connections::keys::key_sensitivity(k)
                == vike_connections::keys::Sensitivity::Secret)
            .count(),
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
/// Asserted against `view::MASKED_FIELD_HINT`'s own bytes rather than a paraphrase, plus the three
/// facts a reader has to come away with, so a rewrite that drops one of them reddens rather than
/// merely reading differently. ⚠ It asserts the text REACHED THE TREE — the hint used to be a
/// `ui.label` nobody had proven renders, and this file's whole reason for existing is that the pure
/// tests cannot see a widget that was never drawn.
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
    // The three facts, each stated in the operator's own terms.
    for phrase in ["never read back", "starts EMPTY", "REPLACE", "KEEP"] {
        assert!(text.contains(phrase), "the hint must still say `{phrase}`: {text}");
    }
    // ...and every SECRET field is still masked. The wording change must never be paid for with
    // the masking, which is what "just show me the value" would actually cost.
    let secrets = text_fields(&h)
        .iter()
        .filter(|f| {
            let key = f.accesskit_node().placeholder().unwrap_or_default().to_string();
            vike_connections::keys::key_sensitivity(&key)
                == vike_connections::keys::Sensitivity::Secret
        })
        .map(|f| f.accesskit_node().role())
        .collect::<Vec<_>>();
    assert_eq!(secrets, vec![Role::PasswordInput; 3], "binance's three API fields stay masked");
}

/// **Dukascopy's DEMO form writes the two login pairs, and names the server SETTING beside them.**
/// Each account's `_SERVER` was a field here until decision 0095's Task 7 made the JForex server the
/// demo tier's `venue.dukascopy.demo.server` setting: a credential row under the old name is read by
/// nothing now, so the form writes none and its note says where the server is set — with the one
/// condition the mount puts on the value (`vike_dukascopy::exec`'s
/// `DukascopyExec::spawn_with_program` keeps it only when it ends in `.jnlp`).
#[test]
fn the_dukascopy_demo_form_writes_the_login_pairs_and_names_the_server_setting() {
    let mut duka = harness("dukascopy");
    duka.run();
    let buttons = edit_buttons(&duka);
    assert_eq!(buttons.len(), 1, "dukascopy offers exactly one editable tier (DEMO)");
    buttons[0].click();
    drop(buttons);
    duka.run();

    let fields = text_fields(&duka);
    let placeholders: Vec<String> = fields
        .iter()
        .map(|f| f.accesskit_node().placeholder().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        placeholders,
        vec![
            "DUKASCOPY_DEMO1_LOGIN",
            "DUKASCOPY_DEMO1_PASSWORD",
            "DUKASCOPY_DEMO2_LOGIN",
            "DUKASCOPY_DEMO2_PASSWORD",
        ],
        "both demo accounts, WHOLE, in the loader's own per-account order"
    );
    // ⚠ NOT "masked like every other one" — that is what this panel used to do and is exactly the
    // half of the owner's report that was a design flaw rather than a missing field. A login NAMES a
    // thing; masking it is how a value an operator set six months ago becomes unknowable from
    // inside the app that wrote it. The password beside it is still masked.
    let roles: Vec<(String, Role)> = fields
        .iter()
        .zip(&placeholders)
        .map(|(f, k)| (k.clone(), f.accesskit_node().role()))
        .collect();
    for (key, role) in &roles {
        let want = match vike_connections::keys::key_sensitivity(key) {
            vike_connections::keys::Sensitivity::Secret => Role::PasswordInput,
            vike_connections::keys::Sensitivity::Public => Role::TextInput,
        };
        assert_eq!(*role, want, "{key}");
    }
    assert_eq!(
        roles.iter().filter(|(_, r)| *r == Role::PasswordInput).count(),
        2,
        "both demo accounts' PASSWORDS are still masked: {roles:?}"
    );

    let text = tree_text(&duka);
    let note = vike_connections::keys::form_note("dukascopy", "DEMO").expect("this cell has one");
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
        "the note is per-cell — binance's form must carry none"
    );
}
