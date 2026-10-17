//! The sentinel suite: no stored SECRET reaches the tree; a readable FIELD shows only in its form.

use std::collections::HashMap;

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::NodeT;
use vike_gateway_panel::keys::{FormPlace, Sensitivity, field_sensitivity, form_fields};
use vike_gateway_panel::{CredentialWrite, gateway_ui};
use vike_model::AccountTier;
use vike_model::feed_status::ConnectionState;
use vike_secrets::CredentialOwner;

use crate::support::{
    create_account, edit_buttons, nodes, scratch_store, seed_fields, store_grids, stored_book,
    test_proc, text_fields, tree_text,
};

// ------------------------------------------------------------------------------------------
// ⚠ THE PROPERTY THAT MATTERS MOST
// ------------------------------------------------------------------------------------------

/// A value no venue name, no field word, no stored name, no account label and no scratch path
/// contains — so a hit is a LEAK and never a coincidence.
const SENTINEL: &str = "zqxjvw7413mfbphgnd8256wu";

/// The shortest run of [`SENTINEL`] this test refuses to find on screen. A pane that rendered every
/// character but the last would pass a bare `contains` check, and a truncated API secret is still
/// an API secret.
const MIN_LEAK_WINDOW: usize = 8;

/// **The SECOND sentinel, and the reason this suite needs one.**
///
/// The schema splits a venue's fields into what AUTHENTICATES (a key, a secret, a token — never
/// read back, always masked) and what NAMES something (a login, an account number, an address),
/// and the second kind is prefilled into an opened form on purpose, because a masked field that
/// starts empty is how a login an operator set six months ago becomes unknowable from inside the
/// app that wrote it.
///
/// So the security property is a CLASSIFICATION and not an absence, and a test that planted one
/// sentinel could only assert the absence. Two sentinels state both halves: this one must APPEAR
/// (in an opened form, and nowhere else), and [`SENTINEL`] must appear NOWHERE.
const PUBLIC_SENTINEL: &str = "kfbrmt5091zwycpldx4738qv";

/// The (venue, field) pairs [`seeded_store`] plants [`PUBLIC_SENTINEL`] in — the readable half.
/// Every other seeded field gets [`SENTINEL`]. ⚠ Asserted equal to the classifier's own verdict
/// (`vike_gateway_panel::keys::field_sensitivity`), so a claim that "a login is readable" cannot be
/// true only because this file said so.
const PUBLIC_FIELDS: [(&str, &str); 2] =
    [("dukascopy", "LOGIN"), ("hyperliquid", "ACCOUNT_ADDRESS")];

/// The fields seeded per (venue, tier, label) account — every shape the panel renders a cell for:
/// the generic API trio, a bespoke login pair, an EIP-712 private key with its address, and one
/// LABELLED account (whose label the panel DOES render — which is exactly why the label and the
/// value are different strings here).
const SEEDED: [(&str, AccountTier, Option<&str>, &[&str]); 5] = [
    ("binance", AccountTier::Live, None, &["API_KEY", "API_SECRET", "API_PASSPHRASE"]),
    ("binance", AccountTier::Demo, None, &["API_KEY", "API_SECRET"]),
    ("dukascopy", AccountTier::Demo, None, &["LOGIN", "PASSWORD"]),
    ("hyperliquid", AccountTier::Live, None, &["PRIVATE_KEY", "ACCOUNT_ADDRESS"]),
    ("binance", AccountTier::Live, Some("HEDGE"), &["API_KEY", "API_SECRET"]),
];

/// A REAL settings database holding [`SEEDED`]: each account created with its tier, then its
/// fields written by FIELD through the record-less test door. ⚠ WHICH sentinel a field gets is
/// decided by the classifier, not by [`PUBLIC_FIELDS`] — that list is then asserted equal to it.
fn seeded_store() -> vike_paths::scratch::ScratchDir {
    let dir = scratch_store("vike-gateway-panel-no-secret");
    let mut public: Vec<(&str, &str)> = Vec::new();
    for (venue, tier, label, fields) in SEEDED {
        let id = create_account(dir.path(), venue, tier, label);
        let pairs: Vec<(&str, &str)> = fields
            .iter()
            .map(|f| match field_sensitivity(venue, f) {
                Sensitivity::Public => {
                    public.push((venue, *f));
                    (*f, PUBLIC_SENTINEL)
                }
                Sensitivity::Secret => (*f, SENTINEL),
            })
            .collect();
        seed_fields(dir.path(), &CredentialOwner::Account(id), &pairs);
    }
    assert_eq!(
        public,
        PUBLIC_FIELDS.to_vec(),
        "the readable half of the seeded store must be exactly what `field_sensitivity` says it is"
    );
    dir
}

fn assert_no_sentinel(h: &Harness<'_, ()>, stage: &str) {
    let text = tree_text(h);
    for start in 0..=SENTINEL.len() - MIN_LEAK_WINDOW {
        let window = &SENTINEL[start..start + MIN_LEAK_WINDOW];
        assert!(
            !text.contains(window),
            "{stage}: a {MIN_LEAK_WINDOW}-character run of a stored credential VALUE ({window}) \
             reached the accessibility tree:\n{text}"
        );
    }
}

/// The panel over `dir`'s store, the grid read the way the binary reads it every frame.
fn panel_over(dir: &std::path::Path, height: f32) -> Harness<'_, ()> {
    let grids = store_grids(dir);
    let live: HashMap<String, ConnectionState> = HashMap::new();
    let creds = CredentialWrite { settings_dir: dir, journal: None, proc: test_proc(), now_ms: 0 };
    let mut h = Harness::builder().with_size(egui::vec2(1000.0, height)).build_ui(move |ui| {
        if vike_ui_theme::harness::type_ready(ui.ctx()) {
            gateway_ui(ui, &grids, &live, creds, None);
        }
    });
    h.run();
    h
}

/// Click the rail button for `venue` when it is not already the selected (label) row.
fn select_venue(h: &mut Harness<'_, ()>, venue: &str) {
    {
        let want = venue.to_string();
        let picks = nodes(h, |n| {
            let a = n.accesskit_node();
            a.role() == Role::Button && a.label().as_deref() == Some(want.as_str())
        });
        assert!(picks.len() <= 1, "a venue appears in the rail once: {}", picks.len());
        if let Some(p) = picks.first() {
            p.click();
        }
    }
    h.run();
}

/// ⚠ **NO STORED VALUE REACHES THE RAIL, THE DETAIL PANE OR AN OPENED EDITOR.**
///
/// This is the pane's first security rule — *"An existing secret's plaintext is NEVER read back
/// into the UI — edit fields always start empty"* — asserted end to end: a real settings database,
/// the grid read from it by `AccountGrids::read` exactly as the binary reads it, and the panel
/// walked through the four states that could each leak differently — the rail, a selected venue's
/// detail pane, an OPENED form, and a LABELLED account's grid.
///
/// ⚠ **Save is never clicked** — the suite's rule, and here the store is real, so it matters more
/// than usual. The scratch directory is self-deleting either way.
#[test]
fn no_stored_credential_value_reaches_the_rail_the_detail_pane_or_an_opened_editor() {
    let dir = seeded_store();
    let stored = stored_book(dir.path());

    // The guard against a test that cannot fail for its stated reason: the value really is in the
    // store and really is what the grid was derived from.
    let grids = store_grids(dir.path());
    assert!(
        grids.labels().any(|l| l.text() == Some("HEDGE")),
        "the labelled account must be enumerated, or half this test is inert"
    );
    let binance = grids.default_grid().iter().find(|r| r.venue == "binance").expect("binance");
    assert!(
        binance.is_complete(AccountTier::Live) && binance.is_complete(AccountTier::Demo),
        "binance's LIVE and DEMO rows must read as complete, or the grid saw nothing"
    );
    let live_row = match &binance.live {
        vike_gateway_panel::TierCell::Account(a) => a.id,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        stored.account(live_row).and_then(|f| f.get("API_SECRET")),
        Some(SENTINEL),
        "the store must actually hold the sentinel"
    );

    let mut h = panel_over(dir.path(), 800.0);
    assert_no_sentinel(&h, "the rail on first paint");

    // 1 — SELECT the venue whose fields are seeded, so the detail pane renders them. ⚠ The selected
    // row is a LABEL, not a button, so there is NO button to click when binance is already the
    // roster's first row; the assertion below is what proves which pane is on screen.
    select_venue(&mut h, "binance");
    let text = tree_text(&h);
    assert!(
        text.contains("API_KEY  stored"),
        "the detail pane must be binance's, and it renders the FIELD and its presence: {text}"
    );
    assert_no_sentinel(&h, "binance's detail pane");

    // 2 — OPEN the LIVE form. Every SECRET buffer must start EMPTY; a read-back is the regression.
    // ⚠ binance's LIVE form is FOUR fields, not three: the API trio plus the venue-wide broker
    // code, a public deployment row the store does not hold here, so it starts empty for a
    // different reason — that it is not masked is the assertion.
    {
        let pencils = edit_buttons(&h);
        assert_eq!(pencils.len(), 2, "binance's default account has a demo and a live row");
        pencils.last().expect("the LIVE row's edit button").click();
    }
    h.run();
    let form = form_fields("binance", FormPlace::Tier(AccountTier::Live));
    let fields = text_fields(&h);
    assert_eq!(
        fields.len(),
        form.len(),
        "the LIVE form is open: the API trio plus the broker code"
    );
    for (f, field) in fields.iter().zip(&form) {
        let a = f.accesskit_node();
        let secret = field.sensitivity() == Sensitivity::Secret;
        assert_eq!(
            a.role(),
            if secret { Role::PasswordInput } else { Role::TextInput },
            "{}: a secret is masked and a name is not",
            field.hint()
        );
        assert_eq!(
            a.value().unwrap_or_default(),
            "",
            "{}: nothing readable is stored for it, so the buffer must be empty — and for a \
             SECRET that holds whatever the store says, because blank means keep",
            field.hint()
        );
    }
    drop(fields);
    assert_no_sentinel(&h, "binance's opened LIVE editor");

    // 3 — the LABELLED account's grid, reached through its chip.
    {
        let picks = nodes(&h, |n| {
            let a = n.accesskit_node();
            a.role() == Role::Button && a.label().as_deref() == Some("HEDGE")
        });
        assert_eq!(picks.len(), 1, "the HEDGE chip is a button until it is selected");
        picks[0].click();
    }
    h.run();
    let text = tree_text(&h);
    assert!(text.contains("HEDGE"), "the account LABEL is rendered — names are not values: {text}");
    assert_no_sentinel(&h, "the HEDGE account's grid");

    // …and Save was never reached: the seeded rows are untouched.
    assert_eq!(stored_book(dir.path()), stored, "this suite never writes the store");
}

/// **THE OTHER HALF OF THE SAME RULE: a NAMING field is read back, and only inside an opened form.**
///
/// ⚠ [`no_stored_credential_value_reaches_the_rail_the_detail_pane_or_an_opened_editor`] asserts an
/// ABSENCE, and an absence is satisfied by a panel that reads nothing back at all — which is the
/// state the owner reported as *"editing Dukascopy credentials doesn't show them"*. This test is
/// the claim that makes that one a classification rather than a blanket: the dukascopy DEMO form
/// PREFILLS the login it already holds, renders it UNMASKED so it can actually be read, and the
/// password beside it is still empty and still a `Role::PasswordInput`.
///
/// ⚠ **The scope is the FORM.** The rail and the detail pane render field words and presence and
/// never values, so the readable sentinel must be absent until the ✏ is clicked — asserted in that
/// order, because "it appears somewhere" would be satisfied by a panel painting the store across
/// the rail.
#[test]
fn a_readable_field_is_prefilled_unmasked_inside_the_form_and_nowhere_else() {
    let dir = seeded_store();
    let stored = stored_book(dir.path());
    let mut h = panel_over(dir.path(), 900.0);
    assert!(
        !tree_text(&h).contains(PUBLIC_SENTINEL),
        "a readable VALUE is not painted on the rail — the rail renders marks"
    );

    // Select dukascopy, then open its one editable cell.
    select_venue(&mut h, "dukascopy");
    assert!(
        !tree_text(&h).contains(PUBLIC_SENTINEL),
        "…nor in the detail pane, which renders only fields and their presence"
    );
    assert_no_sentinel(&h, "dukascopy's detail pane");

    {
        let pencils = edit_buttons(&h);
        assert_eq!(pencils.len(), 1, "dukascopy's default account has one row, at demo");
        pencils[0].click();
    }
    h.run();

    // ⚠ The field each widget writes is read from `form_fields`, NOT from the widget's placeholder:
    // egui files `hint_text` as the accessibility `placeholder` and a PREFILLED field has a value
    // instead, so reading the field off the tree would answer `""` for exactly the fields this test
    // exists to check. Zipping against the form is what keeps the check pointed at the field it
    // means.
    let form = form_fields("dukascopy", FormPlace::Tier(AccountTier::Demo));
    let fields = text_fields(&h);
    assert_eq!(fields.len(), form.len(), "the form renders one widget per field");
    let mut prefilled = 0usize;
    for (f, field) in fields.iter().zip(&form) {
        let a = f.accesskit_node();
        let value = a.value().unwrap_or_default().to_string();
        match field.sensitivity() {
            Sensitivity::Secret => {
                assert_eq!(a.role(), Role::PasswordInput, "{} must stay masked", field.hint());
                assert_eq!(value, "", "{} is a SECRET and must never be read back", field.hint());
            }
            Sensitivity::Public => {
                assert_eq!(
                    a.role(),
                    Role::TextInput,
                    "{} names something — masking it is what made it unreadable",
                    field.hint()
                );
                if PUBLIC_FIELDS.iter().any(|&(v, f)| v == "dukascopy" && f == field.hint()) {
                    assert_eq!(value, PUBLIC_SENTINEL, "{} must show what is held", field.hint());
                    prefilled += 1;
                }
            }
        }
    }
    drop(fields);
    assert_eq!(prefilled, 1, "the login is the one seeded readable field this form offers");

    // The SECRET sentinel is still nowhere, with the form open over the same store.
    assert_no_sentinel(&h, "dukascopy's opened DEMO editor");

    // ...and nothing was written.
    assert_eq!(stored_book(dir.path()), stored, "the store is untouched");
}
