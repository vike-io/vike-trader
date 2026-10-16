//! Shared helpers for the panel suites: harness builders, grid builders (in memory and over a real
//! throwaway store), tree walkers, a throwaway process.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use egui::accesskit::Role;
use egui_kittest::kittest::NodeT;
use egui_kittest::{Harness, Node};
use vike_gateway_panel::{AccountGrids, CredentialWrite, gateway_ui};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::feed_status::ConnectionState;
use vike_paths::scratch::ScratchDir;
use vike_secrets::account_credentials::CredentialBook;
use vike_secrets::account_directory::AccountDirectory;
use vike_secrets::{Account, Accounts, CredentialOwner, CredentialScope};
use vike_ui_theme::appearance::Appearance;

/// The accessible name of `tier_row`'s edit button: what it does, because it shows
/// an icon alone (`vike_ui_theme::icons::named`).
pub(crate) fn pencil() -> String {
    "edit credentials".to_string()
}

/// A settings directory these tests never write, and which no walk produced.
///
/// Relative and deliberately absurd, and it holds no settings database, so a Save that a future
/// edit let this harness reach is REFUSED (there is no store to write) rather than landing in
/// whichever project the working directory happened to sit above.
pub(crate) const NEVER_WRITTEN_SETTINGS_DIR: &str = "a11y-tests-never-save-to-this-settings-dir";

/// A throwaway process identity — these tests assert about the TREE, and no case here reads back
/// which process a written journal record names.
pub(crate) fn test_proc() -> &'static vike_change_journal::Proc {
    static PROC: std::sync::OnceLock<vike_change_journal::Proc> = std::sync::OnceLock::new();
    PROC.get_or_init(|| vike_change_journal::Proc::new("vike-test", 0, "0"))
}

// ------------------------------------------------------------------------------------------
// Grids in memory — account ROWS and their fields, no store on disk
// ------------------------------------------------------------------------------------------

/// One ACTIVE account row of `venue` at `tier`, `label` `None` for the default account.
pub(crate) fn row(id: i64, venue: &str, tier: AccountTier, label: Option<&str>) -> Account {
    Account {
        id,
        venue: venue.to_string(),
        tier,
        label: label.map(str::to_string),
        venue_account_id: None,
        parent_id: None,
        active: true,
        last_verified_at: None,
        max_exposure: None,
    }
}

/// The grid over `venues` that `rows` and `book` fold to — the three store reads, in memory.
pub(crate) fn grids_of(venues: &[&str], rows: Vec<Account>, book: CredentialBook) -> AccountGrids {
    let dir = AccountDirectory::from_rows(Accounts::Known(rows), Some(BTreeMap::new()))
        .with_credentials(book);
    AccountGrids::for_venues(venues, &dir, &HashMap::new())
}

/// ONE venue whose DEFAULT account holds a DEMO row (id 1) and a LIVE row (id 2) and no credential
/// — so a count of ✏ buttons is a count of THAT venue's account rows, and the rail's single row is
/// the SELECTED one, which the detail pane is therefore showing.
pub(crate) fn one_venue(venue: &'static str) -> AccountGrids {
    grids_of(
        &[venue],
        vec![row(1, venue, AccountTier::Demo, None), row(2, venue, AccountTier::Live, None)],
        CredentialBook::new(),
    )
}

/// The harness over [`one_venue`].
pub(crate) fn harness(venue: &'static str) -> Harness<'static, ()> {
    harness_with(venue, HashMap::new())
}

/// The same, with a live-feed status map — the fact the DETAIL pane's `Status` row renders and
/// the rail's dots deliberately do not.
pub(crate) fn harness_with(
    venue: &'static str,
    live: HashMap<String, ConnectionState>,
) -> Harness<'static, ()> {
    harness_grids(one_venue(venue), live)
}

/// The same, over a grid the caller built — the door the sentinel-value suite and the
/// unreadable-store suite come through. The grid carries its own store health.
pub(crate) fn harness_grids(
    grids: AccountGrids,
    live: HashMap<String, ConnectionState>,
) -> Harness<'static, ()> {
    // `journal: None` — these tests assert about the TREE, and a ledger they never write to is one
    // fewer thing that could make them pass for the wrong reason.
    PanelHarness { live, ..PanelHarness::new(grids) }.build()
}

// ------------------------------------------------------------------------------------------
// A REAL throwaway store — for the suites that Save, Remove or read a value back
// ------------------------------------------------------------------------------------------

/// An EMPTY settings database in a self-deleting scratch directory, created the one way a store
/// comes into being (`vike-cli secrets init`'s library half).
pub(crate) fn scratch_store(tag: &str) -> ScratchDir {
    // The system temp directory is legitimate in a test — `crates/vike-ops/tests/hygiene/system_temp_gate.rs`
    // scopes itself to production code. `ScratchDir` is unique per process and self-deleting.
    let dir = ScratchDir::create_in(&std::env::temp_dir(), tag).expect("scratch root");
    vike_secrets::create_store(Some(dir.path().to_str().expect("utf-8 scratch root")))
        .expect("create the empty store");
    dir
}

/// Create an account row with its tier — the operator's act; a credential write never creates one.
pub(crate) fn create_account(
    dir: &Path,
    venue: &str,
    tier: AccountTier,
    label: Option<&str>,
) -> i64 {
    vike_secrets::edit_account_in_unjournalled(
        dir,
        vike_secrets::AccountEdit::Create { venue, tier, label },
    )
    .expect("create the account")
    .after
    .expect("a create returns its row")
    .id
}

/// Write `pairs` (field, value) to `owner` through the record-less test door; returns the NAMES the
/// writer stored them under, in order.
pub(crate) fn seed_fields(
    dir: &Path,
    owner: &CredentialOwner,
    pairs: &[(&str, &str)],
) -> Vec<String> {
    let rows: Vec<(String, String)> =
        pairs.iter().map(|(f, v)| ((*f).to_string(), (*v).to_string())).collect();
    vike_secrets::save_owned_credentials_unjournalled(
        dir,
        owner,
        &rows,
        vike_secrets::UpsertIntent::Add,
    )
    .expect("seed the fields")
    .names
}

/// The store's handle over `dir`, unscoped.
pub(crate) fn store_at(dir: &Path) -> vike_secrets::CredentialStore {
    vike_secrets::CredentialStore::open(
        Some(vike_secrets::StoreLocation::new(dir)),
        CredentialScope::Full,
    )
}

/// The grid the panel's caller would build over `dir`'s store this frame.
pub(crate) fn store_grids(dir: &Path) -> AccountGrids {
    AccountGrids::read(&store_at(dir), &HashMap::new())
}

/// Every account's and venue's credential fields the store holds — what any reader of the box
/// would see, values and all (redacted in `Debug`).
pub(crate) fn stored_book(dir: &Path) -> CredentialBook {
    store_at(dir).credential_book().expect("the credential store reads")
}

/// Account `id`'s TRIMMED value of `field`, as the store holds it.
pub(crate) fn stored_field(dir: &Path, id: i64, field: &str) -> Option<String> {
    stored_book(dir).account(id).and_then(|f| f.get(field)).map(str::to_string)
}

// ------------------------------------------------------------------------------------------
// Tree walkers
// ------------------------------------------------------------------------------------------

/// Every accessibility node matching `pred`, in tree order.
///
/// Walked explicitly rather than through `get_by_*` because a plain label could render the SAME ✏
/// glyph as a plain label, and this test must count BUTTONS. (egui files a `Role::Label`'s text
/// under the node's `value`, not its `label`, so the two are already distinguishable — asserting
/// the role as well means the count cannot start matching the legend if that ever changes.)
pub fn nodes<'t, 'h>(h: &'t Harness<'h, ()>, pred: impl Fn(&Node<'t>) -> bool) -> Vec<Node<'t>> {
    h.root().children_recursive().filter(|n| pred(n)).collect()
}

pub(crate) fn edit_buttons<'t, 'h>(h: &'t Harness<'h, ()>) -> Vec<Node<'t>> {
    nodes(h, |n| {
        let a = n.accesskit_node();
        a.role() == Role::Button && a.label().as_deref() == Some(pencil().as_str())
    })
}

pub(crate) fn text_fields<'t, 'h>(h: &'t Harness<'h, ()>) -> Vec<Node<'t>> {
    nodes(h, |n| {
        matches!(
            n.accesskit_node().role(),
            Role::TextInput | Role::MultilineTextInput | Role::PasswordInput
        )
    })
}

/// Everything the tree says, one node per line — label and value both, because egui files a
/// `Role::Label`'s text under `value` rather than `label`.
pub(crate) fn tree_text(h: &Harness<'_, ()>) -> String {
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

pub(crate) fn password_fields<'t, 'h>(h: &'t Harness<'h, ()>) -> Vec<Node<'t>> {
    nodes(h, |n| n.accesskit_node().role() == Role::PasswordInput)
}

/// The two values typed into a LIVE form by the suites that click Save — `account_editor`'s
/// labelled save and `write_journal`'s journal gate. No six-character run of either appears in a
/// venue name, a tier, a field, a stored name or a label.
///
/// ⚠ Chosen to share no six-character run with anything a `credential_write` line legitimately
/// contains — no `key`, no `secret`, no `live`, no venue name, no digits that appear in
/// `write_journal`'s `T`. That is what lets its `assert_no_secret_window` use a small window
/// without false positives, and it is the difference between a leak test and a coincidence test.
pub(crate) const TYPED_KEY: &str = "zqxjvw7413mfbphgnd8256wu";
pub(crate) const TYPED_SECRET: &str = "pfgzmwqx9042hvbjntdu6531";

pub(crate) fn dry_creds() -> CredentialWrite<'static> {
    CredentialWrite {
        settings_dir: Path::new(NEVER_WRITTEN_SETTINGS_DIR),
        journal: None,
        proc: test_proc(),
        now_ms: 0,
    }
}

/// **The ONE panel harness every suite in this binary opens** — the REAL `gateway_ui` at a
/// window size, under the app's type or a whole installed appearance. Each suite's own constructor
/// (`harness_grids` above, `account_editor`'s `harness` and `harness_preselect`, `layout`'s and
/// `selection_colour`'s `panel`) is a thin wrapper that sets exactly the options it needs.
///
/// [`PanelHarness::new`] carries the defaults; a wrapper overrides fields with `..`.
pub(crate) struct PanelHarness {
    pub(crate) size: egui::Vec2,
    /// The grid — and with it the health of the reads it was folded from.
    pub(crate) grids: AccountGrids,
    pub(crate) live: HashMap<String, ConnectionState>,
    pub(crate) creds: CredentialWrite<'static>,
    /// Asked once per frame that DRAWS, never on the type-binding first frame — so a one-shot
    /// `take` in the closure a wrapper passes is offered on the first drawn frame and never again.
    pub(crate) preselect: Box<dyn FnMut() -> Option<AccountLabel>>,
    /// `Some` installs this whole appearance (`appearance_ready`) INSTEAD of the app's type alone
    /// (`type_ready`) — the theme is what the panel reads its accent from.
    pub(crate) appearance: Option<Appearance>,
    /// Whether [`PanelHarness::build`] settles the panel with one more `run()` before returning it.
    pub(crate) run: bool,
}

impl PanelHarness {
    /// A 1000×800 window, no live feed, [`dry_creds`], no preselection, the app's type, and a
    /// `run()` before the harness is handed back.
    pub(crate) fn new(grids: AccountGrids) -> Self {
        PanelHarness {
            size: egui::vec2(1000.0, 800.0),
            grids,
            live: HashMap::new(),
            creds: dry_creds(),
            preselect: Box::new(|| None),
            appearance: None,
            run: true,
        }
    }

    pub(crate) fn build(self) -> Harness<'static, ()> {
        let PanelHarness { size, grids, live, creds, mut preselect, appearance, run } = self;
        // The app's type (faces AND sizes), so the widths this suite measures are the app's — and
        // bound before the first draw, because the panel draws icons.
        let mut h = Harness::builder().with_size(size).build_ui(move |ui| {
            let ready = match &appearance {
                Some(look) => vike_ui_theme::harness::appearance_ready(ui.ctx(), look),
                None => vike_ui_theme::harness::type_ready(ui.ctx()),
            };
            if ready {
                gateway_ui(ui, &grids, &live, creds, preselect());
            }
        });
        if run {
            h.run();
        }
        h
    }
}
