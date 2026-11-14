//! Reading and driving an `egui_kittest` harness through its ACCESSIBILITY tree, for this crate's
//! kittest suites (`backend_settings_section_rendering`, `data_manager_backend_strip`,
//! `catalog_refresh_screen`, `settings_window`, `store_bar_screen`): each carried its own copy of
//! these. The `screens` binary (`tests/screens.rs`) declares it ONCE, `#[path = "common/a11y.rs"]
//! mod a11y;`, and each suite reads it as `crate::a11y`; a `tests/` SUBDIRECTORY file is not a test
//! target, so this builds no binary of its own. `allow(dead_code)`: a suite uses a subset.
#![allow(dead_code)]

use egui::accesskit::Role;
use egui_kittest::kittest::NodeT;
use egui_kittest::{Harness, Node};
use vike_ui_theme::harness;

/// Every node of `h`'s tree that `pred` accepts, in tree order.
pub fn nodes<'t, 'h>(h: &'t Harness<'h, ()>, pred: impl Fn(&Node<'t>) -> bool) -> Vec<Node<'t>> {
    h.root().children_recursive().filter(|n| pred(n)).collect()
}

/// Click the ONE button labelled `want` and run the harness.
pub fn click(h: &mut Harness<'_, ()>, want: &str) {
    let found = harness::nodes(h, Role::Button, want);
    assert_eq!(found.len(), 1, "exactly one button labelled {want:?} must be on screen");
    found[0].click();
    h.run();
}

/// Where `n` was laid out, when the tree gives it a bounding box.
pub fn node_rect(n: &Node<'_>) -> Option<egui::Rect> {
    n.accesskit_node().bounding_box().map(|_| n.rect())
}

/// Where every node whose text (`vike_ui_theme::harness::node_text`) is exactly `want` was laid out.
pub fn text_rects(h: &Harness<'_, ()>, want: &str) -> Vec<egui::Rect> {
    nodes(h, |_| true)
        .iter()
        .filter(|n| harness::node_text(n) == want)
        .filter_map(node_rect)
        .collect()
}

/// Every accessibility node's rendered text, in tree order — labels file their text under `value`,
/// buttons under `label`.
pub fn texts(h: &Harness<'_, ()>) -> Vec<String> {
    h.root()
        .children_recursive()
        .filter_map(|n| {
            let a = n.accesskit_node();
            a.label().map(|s| s.to_string()).or_else(|| a.value().map(|s| s.to_string()))
        })
        .collect()
}
