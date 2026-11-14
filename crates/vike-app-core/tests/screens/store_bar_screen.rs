//! **The Data Manager's store bar, gated off the accessibility tree.**
//!
//! The bar names the store every destination below it is reading, and nothing in this binary can
//! change that store: the mount is resolved once at startup. So every control the bar draws must
//! be DISABLED and say why on hover — the rule both redesigned screens follow, and the one the
//! Store destination's own actions already obey. A live button whose click goes nowhere is the
//! failure this file exists for.
//!
//! ⚠ **It was not true of the right-aligned other-mount controls.** The picker beside them had
//! already been disabled for exactly this reason, while `archive` and the other store kind (gone
//! since: this build has one kind, the datahub) stayed
//! live `egui::Button`s whose `Response` was dropped with `let _ =`: they invited a click and did
//! nothing. The gate drives the REAL `crates/vike-app-core/src/ui/tool_views/data_rail.rs`'s
//! `store_bar`, because the defect is a renderer drawing a control live, and only the rendered tree
//! can show that.
//!
//! Nothing here touches a store, a socket or a file. `store_bar` renders from `ToolContext` alone, and
//! every field it does not read is the cheapest value its type offers.

use crate::a11y;
use egui::accesskit::Role;
use egui_kittest::kittest::NodeT;
use egui_kittest::{Harness, Node};
use vike_app_core::ui::tool_views::data_rail::{RailCounts, store_bar};

use crate::tool_context::Owned;

/// A resolved datahub address (`ToolContext::history`'s `datahub_addr`). Any `Some` renders the
/// REMOTE-datahub shape of the bar, which is the only thing this file reads it for.
const REMOTE: Option<&str> = Some("127.0.0.1:7878");

/// Frames to run with the pointer resting on a control: egui's default tooltip delay is half a
/// second, and a headless step advances the clock by the harness's quarter-second `step_dt`, so
/// eight steps is four times the delay.
const TOOLTIP_STEPS: usize = 8;

/// One persistent harness over the REAL `store_bar`.
struct Screen {
    harness: Harness<'static, ()>,
}

impl Screen {
    fn new(datahub_addr: Option<&'static str>) -> Self {
        // Everything `ToolContext` borrows, owned, so the harness closure can build one per frame;
        // `store_bar` reads the store fields and nothing else.
        let owned = Owned::new(vike_execution::CoreSnapshot::empty("", ""));
        let harness = Harness::builder().with_size(egui::vec2(1100.0, 200.0)).build_ui(move |ui| {
            // The bar draws a status dot the default faces lack, and egui 0.37's kittest panics on
            // a glyph no installed face draws: the app's type binds the bundled faces.
            if !vike_ui_theme::harness::type_ready(ui.ctx()) {
                return;
            }
            let mut ctx = owned.ctx();
            ctx.history.datahub_addr = datahub_addr;
            store_bar(ui, &ctx, &RailCounts::from_context(&ctx));
        });
        Self { harness }
    }

    fn run(&mut self) {
        self.harness.run();
    }

    /// Every accessibility node's rendered text, in tree order — labels file their text under
    /// `value`, buttons under `label`.
    fn texts(&self) -> Vec<String> {
        a11y::texts(&self.harness)
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

/// **The other-mount control is DISABLED, in either mode** — and with it, nothing the bar draws is
/// live.
///
/// Both modes are driven because the picker's name changes with the mode (`remote datahub`, or
/// `no datahub`), and `archive` is offered by both. No mode offers a `local store`: this build has
/// none, and it was offered as the other kind under a remote mount until 2026-11-12.
#[test]
fn the_other_mount_controls_are_disabled_in_either_mode() {
    for (mode, addr) in [("no datahub", None), ("remote", REMOTE)] {
        let mut screen = Screen::new(addr);
        screen.run();
        let buttons = screen.buttons();
        let hits: Vec<&(String, bool)> =
            buttons.iter().filter(|(label, _)| label.ends_with("archive")).collect();
        assert_eq!(hits.len(), 1, "{mode}: one archive control is drawn: {buttons:?}");
        assert!(
            hits[0].1,
            "{mode}: the archive control is a live button that switches nothing — it must be \
             disabled: {buttons:?}"
        );
        assert!(
            buttons.iter().all(|(label, _)| !label.contains("local store")),
            "{mode}: this build has no local store to offer: {buttons:?}"
        );
        assert!(
            buttons.iter().all(|(_, disabled)| *disabled),
            "{mode}: the mount is resolved at startup, so no control in the store bar may be \
             live: {buttons:?}"
        );
    }
}

/// **A disabled other-mount control SAYS WHY on hover** — the half that makes it disabled rather
/// than merely inert. Its reason must be the true one: the mount is fixed at startup, not a switch
/// that is merely unbuilt.
#[test]
fn an_other_mount_control_says_why_it_cannot_be_pressed() {
    for addr in [None, REMOTE] {
        let mut screen = Screen::new(addr);
        screen.run();
        screen.hover("archive");
        let texts = screen.texts();
        assert!(
            texts.iter().any(|t| t.contains("cannot be switched from here")),
            "hovering the archive control (datahub {addr:?}) must name why it is disabled: \
             {texts:?}"
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
    screen.hover("no datahub");
    let texts = screen.texts();
    assert!(
        texts.iter().any(|t| t.contains("Which store every destination below is reading")),
        "the picker's disabled-hover reason must reach the tree: {texts:?}"
    );
}
