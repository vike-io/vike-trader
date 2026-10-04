//! **The Data Manager's store bar, gated off the accessibility tree.**
//!
//! The bar names the store every destination below it is reading, and nothing in this binary can
//! change that store: the mount is resolved once at startup. So every control the bar draws must
//! be DISABLED and say why on hover — the rule both redesigned screens follow, and the one the
//! Store destination's own actions already obey. A live button whose click goes nowhere is the
//! failure this file exists for.
//!
//! ⚠ **It was not true of the right-aligned other-mount controls.** The picker beside them had
//! already been disabled for exactly this reason, while `archive` and the other store kind stayed
//! live `egui::Button`s whose `Response` was dropped with `let _ =`: they invited a click and did
//! nothing. The gate drives the REAL `crates/vike-app-core/src/ui/tool_views/data_rail.rs`'s
//! `store_bar`, because the defect is a renderer drawing a control live, and only the rendered tree
//! can show that.
//!
//! Nothing here touches a store, a socket or a file. `store_bar` renders from `ToolCtx` alone, and
//! every field it does not read is the cheapest value its type offers.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use egui::accesskit::Role;
use egui_kittest::kittest::NodeT;
use egui_kittest::{Harness, Node};
use vike_app_core::data::catalog_refresh::CatalogRefresh;
use vike_app_core::tools::ToolData;
use vike_app_core::ui::tool_views::data_rail::{RailCounts, store_bar};
use vike_app_core::ui::tool_views::{BookCtx, StoredCtx, ToolCtx};
use vike_model::change_journal::Proc;

/// A `StoredCtx::delete_unavailable` reason. Any `Some` renders the REMOTE-datahub shape of the
/// bar, which is the only thing this file reads it for.
const REMOTE: Option<&str> = Some("a remote datahub serves reads only");

/// Frames to run with the pointer resting on a control: egui's default tooltip delay is half a
/// second, and a headless step advances the clock by the harness's quarter-second `step_dt`, so
/// eight steps is four times the delay.
const TOOLTIP_STEPS: usize = 8;

/// Everything `ToolCtx` borrows, owned, so the harness closure can build one per frame.
struct Owned {
    td: ToolData,
    snap: vike_core::CoreSnapshot,
    textures: HashMap<String, egui::TextureHandle>,
    dsets: vike_data::datasets::Store,
    gaps: vike_data_manager::GapMap,
    partials: vike_data_manager::PartialDayMap,
    statuses: HashMap<String, Arc<Mutex<String>>>,
    proc: Proc,
    catalog: CatalogRefresh,
    symbols: Arc<vike_catalog::Catalog>,
    poly_names: HashMap<String, String>,
}

/// One persistent harness over the REAL `store_bar`.
struct Screen {
    harness: Harness<'static, ()>,
}

impl Screen {
    fn new(delete_unavailable: Option<&'static str>) -> Self {
        // The catalog handle does no I/O at construction and `store_bar` never reads it; the
        // receiver is dropped because nothing will ever send.
        let (tx, _rx) = std::sync::mpsc::channel();
        let owned = Owned {
            td: ToolData::default(),
            snap: vike_core::CoreSnapshot::empty("", ""),
            textures: HashMap::new(),
            dsets: vike_data::datasets::Store::default(),
            gaps: vike_data_manager::GapMap::default(),
            partials: vike_data_manager::PartialDayMap::default(),
            statuses: HashMap::new(),
            proc: Proc::new("vike-test", 4711, "0.1.0"),
            catalog: CatalogRefresh::new(Vec::new(), None, None, None, tx),
            symbols: Arc::new(vike_catalog::Catalog::from_instruments(Vec::new())),
            poly_names: HashMap::new(),
        };
        let harness = Harness::builder().with_size(egui::vec2(1100.0, 200.0)).build_ui(move |ui| {
            let o = &owned;
            let ctx = ToolCtx {
                td: &o.td,
                snap: &o.snap,
                flags: &o.textures,
                logos: &o.textures,
                journal_dir: None,
                feeds: &[],
                dsets: &o.dsets,
                display_tz: vike_chart::DisplayTz::default(),
                stored: StoredCtx {
                    tree: &[],
                    gaps: &o.gaps,
                    partials: &o.partials,
                    loading: false,
                    load_error: None,
                    backfill_status: "",
                    delete_unavailable,
                    partials_note: None,
                    history: None,
                },
                feed_statuses: &o.statuses,
                credentials: vike_connections::CredentialWrite {
                    store: Path::new(""),
                    journal: None,
                    proc: &o.proc,
                    now_ms: 0,
                },
                book: BookCtx { symbol: "", venue: "", book: None, stale: false },
                catalog: &o.catalog,
                symbols: &o.symbols,
                last_trade: None,
                directory: None,
                directory_unavailable: false,
                control_link: vike_app_core::ui::tool_views::ControlLink::Open,
                poly_names: &o.poly_names,
            };
            store_bar(ui, &ctx, &RailCounts::from_ctx(&ctx));
        });
        Self { harness }
    }

    fn run(&mut self) {
        self.harness.run();
    }

    /// Every accessibility node's rendered text, in tree order — labels file their text under
    /// `value`, buttons under `label`.
    fn texts(&self) -> Vec<String> {
        self.harness
            .root()
            .children_recursive()
            .filter_map(|n| {
                let a = n.accesskit_node();
                a.label().map(|s| s.to_string()).or_else(|| a.value().map(|s| s.to_string()))
            })
            .collect()
    }

    /// Every button the bar drew, as `(label, disabled)`, in tree order.
    fn buttons(&self) -> Vec<(String, bool)> {
        self.harness
            .root()
            .children_recursive()
            .filter(|n| n.accesskit_node().role() == Role::Button)
            .map(|n| {
                let a = n.accesskit_node();
                (a.label().map(|s| s.to_string()).unwrap_or_default(), a.is_disabled())
            })
            .collect()
    }

    /// The one button whose label ENDS with `suffix`. The other-mount controls carry a status
    /// glyph in front of their kind, so a suffix names them without spelling the glyph.
    fn button<'t>(&'t self, suffix: &str) -> Node<'t> {
        let mut hits: Vec<Node<'t>> = self
            .harness
            .root()
            .children_recursive()
            .filter(|n| {
                let a = n.accesskit_node();
                a.role() == Role::Button && a.label().is_some_and(|l| l.ends_with(suffix))
            })
            .collect();
        assert_eq!(hits.len(), 1, "exactly one button ends with {suffix:?}: {:?}", self.buttons());
        hits.remove(0)
    }

    /// Rest the pointer on the button ending with `suffix` until egui's tooltip delay has passed.
    fn hover(&mut self, suffix: &str) {
        self.button(suffix).hover();
        self.harness.run_steps(TOOLTIP_STEPS);
    }
}

/// **The other-mount controls are DISABLED, in either mode** — and with them, nothing the bar
/// draws is live.
///
/// Both shapes are driven because the OTHER kind swaps with the mode: a local mount offers
/// `remote datahub`, a remote one offers `local store`, and `archive` is offered by both. A fix that
/// disabled only one arm of that swap would pass a single-mode test.
#[test]
fn the_other_mount_controls_are_disabled_in_either_mode() {
    for (mode, reason, other) in
        [("local", None, "remote datahub"), ("remote", REMOTE, "local store")]
    {
        let mut screen = Screen::new(reason);
        screen.run();
        let buttons = screen.buttons();
        for kind in ["archive", other] {
            let hits: Vec<&(String, bool)> =
                buttons.iter().filter(|(label, _)| label.ends_with(kind)).collect();
            assert_eq!(hits.len(), 1, "{mode} mount: one {kind:?} control is drawn: {buttons:?}");
            assert!(
                hits[0].1,
                "{mode} mount: the {kind:?} control is a live button that switches nothing — it \
                 must be disabled: {buttons:?}"
            );
        }
        assert!(
            buttons.iter().all(|(_, disabled)| *disabled),
            "{mode} mount: the mount is resolved at startup, so no control in the store bar may \
             be live: {buttons:?}"
        );
    }
}

/// **A disabled other-mount control SAYS WHY on hover** — the half that makes it disabled rather
/// than merely inert. Its reason must be the true one: the mount is fixed at startup, not a switch
/// that is merely unbuilt.
#[test]
fn an_other_mount_control_says_why_it_cannot_be_pressed() {
    for (reason, kind) in [(None, "archive"), (None, "remote datahub"), (REMOTE, "local store")] {
        let mut screen = Screen::new(reason);
        screen.run();
        screen.hover(kind);
        let texts = screen.texts();
        assert!(
            texts.iter().any(|t| t.contains("cannot be switched from here")),
            "hovering the {kind:?} control must name why it is disabled: {texts:?}"
        );
    }
}

/// The CONTROL for the test above: the picker — disabled before this file existed — surfaces its
/// reason through the same hover, so a failure above is about the other-mount controls and never
/// about whether the harness can see a disabled control's hover text at all.
#[test]
fn the_picker_says_why_it_cannot_be_pressed() {
    let mut screen = Screen::new(None);
    screen.run();
    screen.hover("local store");
    let texts = screen.texts();
    assert!(
        texts.iter().any(|t| t.contains("Which store every destination below is reading")),
        "the picker's disabled-hover reason must reach the tree: {texts:?}"
    );
}
