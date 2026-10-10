//! Shared helpers for the panel suites: harness builders, tree walkers, a throwaway process.

use std::collections::HashMap;
use std::path::Path;

use egui::accesskit::Role;
use egui_kittest::kittest::NodeT;
use egui_kittest::{Harness, Node};
use vike_connections::{
    AccountGrids, CredentialWrite, StoreHealth, VenueCredStatus, connections_ui,
};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::feed_status::ConnectionState;
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
pub(crate) fn test_proc() -> &'static vike_model::change_journal::Proc {
    static PROC: std::sync::OnceLock<vike_model::change_journal::Proc> = std::sync::OnceLock::new();
    PROC.get_or_init(|| vike_model::change_journal::Proc::new("vike-test", 0, "0"))
}

/// One venue's row only, so a count of ✏ buttons is a count of THAT venue's editable tiers — and
/// so the rail's single row is the SELECTED one, which the detail pane is therefore showing.
pub(crate) fn harness(venue: &'static str) -> Harness<'static, ()> {
    harness_with(venue, HashMap::new())
}

/// The same, with a live-feed status map — the fact the DETAIL pane's `Status` row renders and
/// the rail's dots deliberately do not.
pub(crate) fn harness_with(
    venue: &'static str,
    live: HashMap<String, ConnectionState>,
) -> Harness<'static, ()> {
    let grids = AccountGrids::single(vec![VenueCredStatus {
        venue: venue.to_string(),
        sim: false,
        demo: false,
        live: false,
    }]);
    harness_grids(grids, live, StoreHealth::Readable)
}

/// The same, over a grid and a store health the caller built — the door the sentinel-value suite
/// and the unreadable-store suite come through.
pub(crate) fn harness_grids(
    grids: AccountGrids,
    live: HashMap<String, ConnectionState>,
    health: StoreHealth,
) -> Harness<'static, ()> {
    // `journal: None` — these tests assert about the TREE, and a ledger they never write to is one
    // fewer thing that could make them pass for the wrong reason.
    PanelHarness { live, health, ..PanelHarness::new(grids) }.build()
}

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
/// venue name, a tier, a key name or a label.
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

/// **The ONE panel harness every suite in this binary opens** — the REAL `connections_ui` at a
/// window size, under the app's type or a whole installed appearance. Each suite's own constructor
/// (`harness_grids` above, `account_editor`'s `harness` and `harness_preselect`, `layout`'s and
/// `selection_colour`'s `panel`) is a thin wrapper that sets exactly the options it always set, so
/// no test body changed when the five copies became this one.
///
/// [`PanelHarness::new`] carries the defaults; a wrapper overrides fields with `..`.
pub(crate) struct PanelHarness {
    pub(crate) size: egui::Vec2,
    pub(crate) grids: AccountGrids,
    pub(crate) live: HashMap<String, ConnectionState>,
    pub(crate) health: StoreHealth,
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
    /// A 1000×800 window, no live feed, a readable store, [`dry_creds`], no preselection, the
    /// app's type, and a `run()` before the harness is handed back.
    pub(crate) fn new(grids: AccountGrids) -> Self {
        PanelHarness {
            size: egui::vec2(1000.0, 800.0),
            grids,
            live: HashMap::new(),
            health: StoreHealth::Readable,
            creds: dry_creds(),
            preselect: Box::new(|| None),
            appearance: None,
            run: true,
        }
    }

    pub(crate) fn build(self) -> Harness<'static, ()> {
        let PanelHarness { size, grids, live, health, creds, mut preselect, appearance, run } =
            self;
        // The app's type (faces AND sizes), so the widths this suite measures are the app's — and
        // bound before the first draw, because the panel draws icons.
        let mut h = Harness::builder().with_size(size).build_ui(move |ui| {
            let ready = match &appearance {
                Some(look) => vike_ui_theme::harness::appearance_ready(ui.ctx(), look),
                None => vike_ui_theme::harness::type_ready(ui.ctx()),
            };
            if ready {
                connections_ui(ui, &grids, &live, &health, creds, preselect());
            }
        });
        if run {
            h.run();
        }
        h
    }
}
