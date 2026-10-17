//! **The Gateway destination's REAL credential panel, drawn at the width Data Manager
//! actually gives it, and CLICKED.**
//!
//! Replaces `connections_window.rs` (deleted this task, Connections-merges-into-Data-Manager),
//! which drove this same `vike_gateway_panel::gateway_ui` panel inside the now-deleted
//! standalone Connections window's chrome (`connections_body`, a real `tool_title_bar`). That
//! chrome is gone — `crates/vike-app-core/src/ui/tool_views/data.rs`'s `DataDest::Gateway` arm
//! calls `gateway_ui` directly now, inside Data Manager's own rail/body split — so this suite
//! drives the panel the same way: no title bar, no `connections_body`, just the panel at the real
//! WIDTH that split leaves it.
//!
//! ⚠ **The width is computed, not guessed, and it is narrower than the old window ever gave this
//! panel.** `data_tool_content`'s body allocation is
//! `(full - rail_w - RAIL_SEPARATOR_RESERVE).max(BODY_MIN_W)` where
//! `rail_w = RAIL_W.min(full * RAIL_MAX_FRAC)` — see [`credentials_panel_width`], which replicates
//! that arithmetic from the same public `vike_ui_theme::value::data` constants `data.rs` reads,
//! with a comment pointing back at the real call site, since nothing wires the two together
//! automatically. At the shipped 560pt tool-window width that formula gives 368pt; the old
//! standalone window handed this panel close to the full 560pt. So the regression this suite
//! exists for — `rail_chips` blowing the venue rail up to ~700pt and pushing every edit button
//! past the clip rect, first caught by `connections_window.rs` — is, if anything, EASIER to reopen
//! inside Data Manager than it was in the window this suite used to drive, which is why the
//! coverage moves rather than lapses.
//!
//! ⚠ **No credential VALUE is rendered, logged or persisted anywhere but the throwaway store** —
//! same discipline, same reason, as the file this replaces and as
//! `crates/vike-gateway-panel/tests/panel/write_journal.rs`'s.

use egui::accesskit::Role;
use egui_kittest::kittest::NodeT;
use egui_kittest::{Harness, Node};
use std::collections::HashMap;
use vike_gateway_panel::{AccountGrids, CredentialWrite, gateway_ui};
use vike_model::AccountTier;
use vike_model::feed_status::ConnectionState;
use vike_paths::scratch::ScratchDir;
use vike_secrets::account_credentials::CredentialBook;
use vike_secrets::account_directory::AccountDirectory;
use vike_secrets::{Account, Accounts};
use vike_ui_theme::value::data;
use vike_ui_theme::value::workspace::TOOL_WINDOW_SIZE;

/// The accessible name of `vike_gateway_panel::view`'s `tier_row` edit button: what it does,
/// because it shows an icon alone (`vike_ui_theme::icons::named`).
fn pencil() -> String {
    "edit credentials".to_string()
}

/// The two values typed into the LIVE form. Chosen to share no six-character run with any venue
/// name, field, tier word or path this panel renders — a hit is a leak, never a coincidence.
const TYPED_KEY: &str = "zqxjvw7413mfbphgnd8256wu";
const TYPED_SECRET: &str = "pfgzmwqx9042hvbjntdu6531";

/// The venue whose LIVE form this suite drives — the generic API trio, and first in
/// `vike_model::VENUES`, so it is also the venue the panel selects on frame one.
const VENUE: &str = "binance";

/// **The body width `data_tool_content` actually gives `gateway_ui`, at a given window width.**
///
/// Replicates `crates/vike-app-core/src/ui/tool_views/data.rs`'s `data_tool_content`:
/// `let rail_w = data::RAIL_W.min(full * data::RAIL_MAX_FRAC); … vec2((full - rail_w -
/// data::RAIL_SEPARATOR_RESERVE).max(data::BODY_MIN_W), body_h)`. Not derivable automatically from
/// here (that arithmetic is inline in a private function), so if it changes, this needs a matching
/// edit — the alternative, driving the real `data_tool_content` through a full `ToolCtx`, is real
/// future coverage this task did not build (see the module doc).
fn credentials_panel_width(window_w: f32) -> f32 {
    let rail_w = data::RAIL_W.min(window_w * data::RAIL_MAX_FRAC);
    (window_w - rail_w - data::RAIL_SEPARATOR_RESERVE).max(data::BODY_MIN_W)
}

// ------------------------------------------------------------------------------------------
// Tree helpers — the same shapes `connections_window.rs` used.
// ------------------------------------------------------------------------------------------

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

fn node_text(n: &Node<'_>) -> String {
    let a = n.accesskit_node();
    match (a.label(), a.value()) {
        (Some(l), _) if !l.is_empty() => l.to_string(),
        (_, Some(v)) => v.to_string(),
        _ => String::new(),
    }
}

fn tree_text(h: &Harness<'_, ()>) -> String {
    h.root().children_recursive().map(|n| node_text(&n)).collect::<Vec<_>>().join("\n")
}

fn rects_of(h: &Harness<'_, ()>, want: &str) -> Vec<egui::Rect> {
    nodes(h, |_| true)
        .iter()
        .filter(|n| node_text(n) == want)
        .filter_map(|n| n.accesskit_node().bounding_box().map(|_| n.rect()))
        .collect()
}

/// Which venue the rail has selected, read off the ROLE: the SELECTED row is a `Label`, every
/// other one a `Button`.
fn shown_venue(h: &Harness<'_, ()>) -> Option<String> {
    let buttons: Vec<String> =
        nodes(h, |n| n.accesskit_node().role() == Role::Button).iter().map(node_text).collect();
    vike_model::VENUES.iter().find(|v| !buttons.iter().any(|b| b == *v)).map(|v| (*v).to_string())
}

// ------------------------------------------------------------------------------------------
// The harness: the REAL credential panel, at the width Data Manager actually hands it.
// ------------------------------------------------------------------------------------------

/// The whole roster with the selected venue's DEFAULT account holding a DEMO and a LIVE row and
/// no credential — every rail chip, and the detail pane's edit controls, with nothing stored.
fn roster_grids() -> AccountGrids {
    let row = |id: i64, tier: AccountTier| Account {
        id,
        venue: VENUE.to_string(),
        tier,
        label: None,
        venue_account_id: None,
        parent_id: None,
        active: true,
        last_verified_at: None,
        max_exposure: None,
    };
    let dir = AccountDirectory::from_rows(
        Accounts::Known(vec![row(1, AccountTier::Demo), row(2, AccountTier::Live)]),
        Some(Default::default()),
    )
    .with_credentials(CredentialBook::new());
    AccountGrids::from_directory(&dir, &HashMap::new())
}

/// A live `gateway_ui` panel sized `size` over `grids`, with no outer chrome at all — the panel
/// IS the root `Ui`, which is the honest shape: Data Manager wraps it in nothing but an
/// `allocate_ui_with_layout` of this exact size (the crumb/strip/rule above it affect only what is
/// ABOVE this rect, never its width or height).
fn window(size: egui::Vec2, grids: AccountGrids, creds: CredentialWrite<'_>) -> Harness<'_, ()> {
    let live: HashMap<String, ConnectionState> = HashMap::new();
    let mut h = Harness::builder().with_size(size).build_ui(move |ui| {
        if !vike_ui_theme::harness::type_ready(ui.ctx()) {
            return;
        }
        gateway_ui(ui, &grids, &live, creds, None);
    });
    h.run();
    h
}

const NEVER_WRITTEN_SETTINGS_DIR: &str = "data-manager-credentials-panel-tests-never-save-here";

fn test_proc() -> &'static vike_change_journal::Proc {
    static PROC: std::sync::OnceLock<vike_change_journal::Proc> = std::sync::OnceLock::new();
    PROC.get_or_init(|| vike_change_journal::Proc::new("vike-test", 0, "0"))
}

fn dry_creds() -> CredentialWrite<'static> {
    CredentialWrite {
        settings_dir: std::path::Path::new(NEVER_WRITTEN_SETTINGS_DIR),
        journal: None,
        proc: test_proc(),
        now_ms: 0,
    }
}

// ------------------------------------------------------------------------------------------
// 1 — the chips
// ------------------------------------------------------------------------------------------

/// **Every venue chip is clickable at the width Data Manager gives this panel in its shipped
/// tool-window size.** See the module doc for why that width is narrower than the old standalone
/// window's and therefore at least as exacting a test.
#[test]
fn every_venue_chip_is_clickable_at_the_shipped_destination_width() {
    every_chip_selects_at(egui::vec2(credentials_panel_width(TOOL_WINDOW_SIZE.x), 700.0));
}

/// The same walk at the narrowest supported window width (400pt, same figure
/// `connections_window.rs` used for the standalone window) — re-derived through the same body-width
/// formula, so the panel under test is narrower still.
#[test]
fn every_venue_chip_is_clickable_at_the_narrowest_supported_width() {
    every_chip_selects_at(egui::vec2(credentials_panel_width(400.0), 700.0));
}

fn every_chip_selects_at(size: egui::Vec2) {
    let creds = dry_creds();
    let mut h = window(size, roster_grids(), creds);

    let first = shown_venue(&h).expect("the panel selects a venue on frame one");
    let mut unreachable: Vec<String> = Vec::new();
    for venue in vike_model::VENUES {
        if *venue == first {
            continue;
        }
        {
            let chips = buttons_labelled(&h, venue);
            assert_eq!(
                chips.len(),
                1,
                "{size:?}: exactly one rail chip is a button for {venue}: {}",
                tree_text(&h)
            );
            chips[0].click();
        }
        // Two frames: `gateway_ui` applies the pick after the rail/detail block has already
        // been laid out this frame, so the frame that consumes the click still renders the
        // previous selection.
        h.run();
        h.run();
        if shown_venue(&h).as_deref() != Some(*venue) {
            unreachable.push((*venue).to_string());
        }
    }
    assert!(
        unreachable.is_empty(),
        "clicking these rail chips changed nothing — they are rendered outside the window's clip \
         rect, which is what makes `egui::Ui::interact` record an empty `interact_rect` and the \
         hit test drop them: {unreachable:?}"
    );
}

// ------------------------------------------------------------------------------------------
// 2 — the geometry
// ------------------------------------------------------------------------------------------

/// **The rail is a rail, and the edit controls land inside a reasonable height** — at the
/// shipped destination width, which is narrower than the old standalone window ever gave this
/// panel. Both numbers come off the rendered tree, not a pinned pixel count.
///
/// ⚠ **The harness height is GENEROUS (900pt), not the old file's 400pt, and that is not loosening
/// this gate — it is correcting an assumption that no longer holds.** Data Manager's windows
/// auto-size to their content on the height axis exactly like the old standalone window did
/// (`crates/vike-app-core/src/ui/workspace/state/chrome.rs`'s `show_window` builds every tool window
/// `.resizable(false)` and lets `egui::Resize` report `size.y = last_content_size.y`), so a
/// NARROWER width legitimately needing a TALLER one-column layout is not a reachability defect —
/// the real window simply grows to show it, the same way it already does for every other
/// destination. What the old 400pt-height gate was actually protecting against was the
/// HISTORICAL defect's SHAPE — a rail that blows up disproportionately (~700pt for a handful of
/// chips, a wrapping row nested inside a wrapping row) rather than one that is merely taller
/// because its WIDTH is narrower. The ceilings below are qualitative for that reason: generous
/// enough to admit the narrower-width layout, tight enough to still catch the historical shape.
#[test]
fn the_rail_and_the_edit_controls_fit_the_shipped_destination_width() {
    let creds = dry_creds();
    let size = egui::vec2(credentials_panel_width(TOOL_WINDOW_SIZE.x), 900.0);
    let h = window(size, roster_grids(), creds);

    let chip_rects: Vec<egui::Rect> =
        vike_model::VENUES.iter().flat_map(|v| rects_of(&h, v)).collect();
    assert!(!chip_rects.is_empty(), "the rail rendered no venue at all: {}", tree_text(&h));
    let top = chip_rects.iter().map(|r| r.top()).fold(f32::INFINITY, f32::min);
    let bottom = chip_rects.iter().map(|r| r.bottom()).fold(f32::NEG_INFINITY, f32::max);
    let rail_h = bottom - top;
    // ~40pt per venue is a roomy single-column row at this text size; the historical defect
    // measured ~700pt for the SAME roster (a chip alone was 143pt tall). This ceiling admits a
    // flat one-column list with margin to spare and still catches that shape returning.
    let sane_ceiling = vike_model::VENUES.len() as f32 * 40.0;
    assert!(
        rail_h < sane_ceiling,
        "the venue rail is {rail_h:.0}pt tall for {} venues — it is not a rail, it is a wrapping \
         row nested inside a wrapping row, and everything after it is pushed where no click \
         reaches it. See `vike_gateway_panel::view`'s `rail_chips`.",
        vike_model::VENUES.len()
    );

    let pencils = buttons_labelled(&h, &pencil());
    assert!(!pencils.is_empty(), "the detail pane renders no edit control: {}", tree_text(&h));
    let first = pencils
        .iter()
        .filter_map(|n| n.accesskit_node().bounding_box().map(|_| n.rect()))
        .map(|r| r.top())
        .fold(f32::INFINITY, f32::min);
    assert!(
        first < size.y,
        "the first edit control is laid out at y = {first:.0} in a {:.0}pt-tall harness — below \
         even the GENEROUS floor this gate gives it room against",
        size.y
    );
}

// ------------------------------------------------------------------------------------------
// 3 — the act
// ------------------------------------------------------------------------------------------

/// The height gate 2 drives — genuinely taller than its usual room, because a credential form
/// expanded makes the real panel taller than that, and in the real window that is reached by
/// SCROLLING (Data Manager's body sits inside the same scroll area the old window used), which a
/// bare `Harness` has no equivalent of. So this gate fixes the WIDTH (the axis the regression
/// lived on) and gives the HEIGHT enough room that the whole act is visible.
const FLOW_SIZE_H: f32 = 1600.0;

/// **Click the pencil, the form opens, type, Save, the bytes are on disk — all of it at the width
/// Data Manager's Gateway destination actually renders this panel at.**
///
/// The store is a THROWAWAY `ScratchDir`: nothing in this workspace may rewrite the user's only
/// copy of live venue keys, which is exactly why `gateway_ui` takes the settings directory as a
/// parameter.
#[test]
fn clicking_the_edit_control_opens_the_form_and_save_reaches_the_store() {
    let root = ScratchDir::create_in(&std::env::temp_dir(), "vike-data-manager-credentials-panel")
        .expect("scratch root");
    // The settings DATABASE is the only credential store, so the Save needs one to land in —
    // created EMPTY the one way a store comes into being, `vike-cli secrets init`'s
    // library half.
    vike_secrets::create_store(root.path().to_str()).expect("create the empty store");
    // The account first, with its tier — a credential write never creates one (decision 0156
    // item 6) — so the Save below lands on THIS row, by field.
    let id = vike_secrets::edit_account_in_unjournalled(
        root.path(),
        vike_secrets::AccountEdit::Create { venue: VENUE, tier: AccountTier::Live, label: None },
    )
    .expect("create the live account")
    .after
    .expect("a create returns its row")
    .id;
    let creds =
        CredentialWrite { settings_dir: root.path(), journal: None, proc: test_proc(), now_ms: 0 };
    let size = egui::vec2(credentials_panel_width(TOOL_WINDOW_SIZE.x), FLOW_SIZE_H);
    let store = vike_secrets::CredentialStore::open(
        Some(vike_secrets::StoreLocation::new(root.path())),
        vike_secrets::CredentialScope::Full,
    );
    let mut h = window(size, AccountGrids::read(&store, &HashMap::new()), creds);

    assert_eq!(
        shown_venue(&h).as_deref(),
        Some(VENUE),
        "the panel opens on the roster's first venue: {}",
        tree_text(&h)
    );

    // The default account's one row is at LIVE, so its pencil is the only one.
    {
        let pencils = buttons_labelled(&h, &pencil());
        assert_eq!(pencils.len(), 1, "{VENUE}'s one account row, at live: {}", tree_text(&h));
        pencils.last().expect("an edit button").click();
    }
    h.run();

    let fields = password_fields(&h);
    let n = fields.len();
    drop(fields);
    assert_eq!(
        n,
        3,
        "⚠ THE DEFECT: clicking the pencil opened NO form. The button is in the tree and the click \
         reached nothing, which is what an `interact_rect` clipped to nothing looks like from \
         here. Tree:\n{}",
        tree_text(&h)
    );
    assert!(
        tree_text(&h).contains(&format!("Edit {VENUE} / live")),
        "…and the form that opened is the LIVE row's: {}",
        tree_text(&h)
    );

    for (i, value) in [TYPED_KEY, TYPED_SECRET].into_iter().enumerate() {
        {
            let fields = password_fields(&h);
            fields[i].focus();
        }
        h.run();
        {
            let fields = password_fields(&h);
            fields[i].type_text(value);
        }
        h.run();
    }

    {
        let save = buttons_labelled(&h, "Save");
        assert_eq!(save.len(), 1, "exactly one Save button is on screen: {}", tree_text(&h));
        save[0].click();
    }
    h.run();

    assert!(
        password_fields(&h).is_empty(),
        "the form must close on a successful save — it is still open, so Save failed: {}",
        tree_text(&h)
    );

    let book = store.credential_book().expect("the credential store reads");
    let row = book.account(id).expect("the Save landed on the account row");
    assert_eq!(row.get("API_KEY"), Some(TYPED_KEY));
    assert_eq!(row.get("API_SECRET"), Some(TYPED_SECRET));
    assert_eq!(row.get("API_PASSPHRASE"), None, "a blank field means leave unchanged");
}
