//! Accessibility-tree tests for the credential grid + masked edit form
//! ([`vike_connections::connections_ui`]).
//!
//! `view.rs`'s own tests all call the pure `edit_fields` helper directly; the RENDER was untested,
//! which left three of the module's stated rules asserted nowhere:
//!
//! 1. **"Edit fields are `egui::TextEdit::password(true)` (masked)"** — security rule 3 of the
//!    module doc, and the test only an accessibility tree can make. `.password(true)` is what
//!    makes egui file the widget as `Role::PasswordInput` instead of `Role::TextInput`, and the
//!    same call is what masks the value handed to assistive tech. Dropping it compiles, renders a
//!    normal-looking field, keeps every pure test green — and reads a live API secret aloud.
//! 2. **The ✎ opens the cell it sits in.** `status_cell` is called three times per row with only
//!    the tier string differing; an off-by-one would open the SIM form from the LIVE cell and save
//!    a live key into a sim var. The placeholders assert which form actually opened.
//! 3. **A tier with no credentials to write offers no edit affordance.** `status_cell` renders the
//!    ✎ only when `edit_fields(venue, tier)` is non-empty. The pure tests pin what `edit_fields`
//!    RETURNS; nothing pinned that the view still consults it, so a `status_cell` that stopped
//!    asking would open a form writing `DUKASCOPY_SIM_*` vars no bridge reads.
//!
//! ⚠ These tests never click **Save** — [`NEVER_WRITTEN_STORE`] is the belt that says so. The
//! braces are that `connections_ui` no longer resolves the store for itself: the path is now a
//! `vike_connections::CredentialWrite` parameter, so a harness can point it anywhere and the real
//! `workspace_dotenv_path()` is unreachable from here. Nothing in this workspace may rewrite the
//! user's only copy of live venue keys (CLAUDE.md, "Credentials & the live gate"). Opening the
//! form and reading the tree is the whole interaction — no secret is typed, so none can leak.
//! (The Save arm's own end-to-end gate lives in `tests/credential_write_journal.rs`, which drives
//! it against a throwaway store and a throwaway ledger.)

use std::collections::HashMap;
use std::path::Path;

use egui::accesskit::Role;
use egui_kittest::kittest::NodeT;
use egui_kittest::{Harness, Node};
use vike_connections::{
    connections_ui, AccountGrids, ConnectionState, CredentialWrite, VenueCredStatus,
};

/// The glyph `status_cell` labels its edit button with.
const PENCIL: &str = "\u{270E}";

/// A store path these tests never write, and which no walk produced.
///
/// Relative and deliberately absurd: if a future edit made this file's harness reach the Save arm,
/// the failure is a stray file in the crate directory that review would notice, not a mutation of
/// whoever's `secrets.env` the working directory happened to sit above.
const NEVER_WRITTEN_STORE: &str = "a11y-tests-never-save-to-this.env";

/// One venue's row only, so a count of ✎ buttons is a count of THAT venue's editable tiers.
fn harness(venue: &'static str) -> Harness<'static, ()> {
    let grids = AccountGrids::single(vec![VenueCredStatus {
        venue: venue.to_string(),
        sim: false,
        demo: false,
        live: false,
    }]);
    let live: HashMap<String, ConnectionState> = HashMap::new();
    // `journal: None` — these tests assert about the TREE, and a ledger they never write to is one
    // fewer thing that could make them pass for the wrong reason.
    let creds = CredentialWrite { store: Path::new(NEVER_WRITTEN_STORE), journal: None, now_ms: 0 };
    Harness::builder().with_size(egui::vec2(1000.0, 800.0)).build_ui(move |ui| {
        connections_ui(ui, &grids, &live, creds, None);
    })
}

/// Every accessibility node matching `pred`, in tree order.
///
/// Walked explicitly rather than through `get_by_*` because the header legend renders the SAME ✎
/// glyph as a plain label, and this test must count BUTTONS. (egui files a `Role::Label`'s text
/// under the node's `value`, not its `label`, so the two are already distinguishable — asserting
/// the role as well means the count cannot start matching the legend if that ever changes.)
fn nodes<'t>(h: &'t Harness<'static, ()>, pred: impl Fn(&Node<'t>) -> bool) -> Vec<Node<'t>> {
    h.root().children_recursive().filter(|n| pred(n)).collect()
}

fn edit_buttons<'t>(h: &'t Harness<'static, ()>) -> Vec<Node<'t>> {
    nodes(h, |n| {
        let a = n.accesskit_node();
        a.role() == Role::Button && a.label().as_deref() == Some(PENCIL)
    })
}

fn text_fields<'t>(h: &'t Harness<'static, ()>) -> Vec<Node<'t>> {
    nodes(h, |n| {
        matches!(
            n.accesskit_node().role(),
            Role::TextInput | Role::MultilineTextInput | Role::PasswordInput
        )
    })
}

/// The edit affordance appears exactly on the tiers a venue can actually store keys for.
///
/// binance has all three (generic `_API_KEY`/`_API_SECRET`/`_API_PASSPHRASE` at Sim/Demo/Live);
/// dukascopy has ONLY Demo — no Sim or Live tier exists for it, so those two cells must stay
/// dot-only. Reddens on `status_cell` dropping its `edit_fields(...).is_empty()` guard, which the
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
/// entitled to speak a typed API secret out loud), and on `status_cell` passing the wrong tier.
#[test]
fn an_opened_credential_form_is_masked_and_belongs_to_the_cell_that_was_clicked() {
    let mut h = harness("binance");
    h.run();
    assert!(text_fields(&h).is_empty(), "no form is open before the ✎ is clicked");

    // Row order is Sim, Demo, Live — click the LAST cell's ✎ and the LIVE form must open.
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
        vec!["BINANCE_LIVE_API_KEY", "BINANCE_LIVE_API_SECRET", "BINANCE_LIVE_API_PASSPHRASE"],
        "the LIVE cell's ✎ must open the LIVE form, in `edit_fields` order"
    );

    for (f, key) in fields.iter().zip(&placeholders) {
        assert_eq!(
            f.accesskit_node().role(),
            Role::PasswordInput,
            "{key} must be masked — TextEdit::password(true)"
        );
    }
}
