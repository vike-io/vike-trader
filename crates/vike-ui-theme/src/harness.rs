//! [`type_ready`] — the ONE first-frame guard a headless `egui_kittest` harness calls before it
//! draws anything in a family only the bundled set binds: an icon ([`crate::icons::FAMILY`]) or a
//! named weight ([`crate::fonts::SEMIBOLD`] and its siblings).
//!
//! `egui_kittest` runs one frame INSIDE `Harness::builder().build_ui(…)`, on a context holding
//! egui's DEFAULT fonts, before a test can touch that context — and a family those fonts do not
//! bind makes epaint PANIC (`FontFamily::Name("icons") is not bound to any fonts`).
//! `Context::set_fonts` takes effect at the start of the NEXT pass, so installing the type after
//! the build is too late for that first frame. This guard installs the app's type
//! ([`crate::appearance::install_type`]: the bundled faces AND the role sizes) the first time a
//! context asks, answers `false` so the harness draws nothing that frame, and requests a repaint;
//! every later call answers `true`. A harness that is built and then `run()` therefore asserts
//! against a frame drawn with the fonts the app ships.
//!
//! It lives here, behind `test-support`, for the reason `frame_sanity` does: every GUI crate sits
//! above this crate, so one copy serves all of them. Do not copy it into a test file.
//!
//! Its twin [`appearance_ready`] installs a whole appearance (theme, market colours, density, text
//! size) instead of the type alone, for a harness that tests a surface under one.
//!
//! [`node_text`], [`tree_text`] and [`tree_node_texts`] read the accessibility tree a harness built back
//! as text, ONE copy for every GUI crate's a11y tests (vike-app-core's and vike-gateway-panel's carried
//! their own until 2026-10-31); [`nodes`] and [`button_labels`] find the controls the same way.

/// Whether the app's type is bound on `ctx` for THIS frame — installing it, and answering `false`,
/// the first time a context asks. Call it at the top of a harness's UI closure and draw nothing
/// until it answers `true`.
pub fn type_ready(ctx: &egui::Context) -> bool {
    installed_once(ctx, egui::Id::unique("vike_ui_theme::harness::type_ready"), || {
        crate::appearance::install_type(ctx, crate::type_scale::TextSize::Small);
    })
}

/// [`type_ready`]'s twin for a harness that tests a surface UNDER an appearance — a theme, a market
/// set, a density, a text size. The first time a context asks, it installs `a` with
/// [`crate::appearance::install`]: the bundled faces, and everything `apply` sets, kept where
/// `appearance::current` reads it. It answers `false` that frame and requests a repaint; every later
/// call answers `true`.
pub fn appearance_ready(ctx: &egui::Context, a: &crate::appearance::Appearance) -> bool {
    installed_once(ctx, egui::Id::unique("vike_ui_theme::harness::appearance_ready"), || {
        crate::appearance::install(ctx, a);
    })
}

/// The first time `ctx` asks under `id`: run `install`, request a repaint and answer `false`. Every
/// later call answers `true`.
fn installed_once(ctx: &egui::Context, id: egui::Id, install: impl FnOnce()) -> bool {
    if ctx.data(|d| d.get_temp::<bool>(id)).unwrap_or(false) {
        return true;
    }
    install();
    ctx.data_mut(|d| d.insert_temp(id, true));
    ctx.request_repaint();
    false
}

/// One accessibility node's text, whatever role carries it: its label when that is non-empty, else
/// its value (egui files a `Role::Label`'s text under `value`), else nothing.
pub fn node_text(n: &egui_kittest::Node<'_>) -> String {
    use egui_kittest::kittest::NodeT as _;
    let a = n.accesskit_node();
    match (a.label(), a.value()) {
        (Some(l), _) if !l.is_empty() => l.to_string(),
        (_, Some(v)) => v.to_string(),
        _ => String::new(),
    }
}

/// Everything the tree says, one node per line — label AND value both (a label, a space, then the
/// value), because egui files a `Role::Label`'s text under `value` rather than `label`.
pub fn tree_text<S>(h: &egui_kittest::Harness<'_, S>) -> String {
    use egui_kittest::kittest::NodeT as _;
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

/// The tree as [`node_text`] reads each node, one node per line (one text per node, where
/// [`tree_text`] gives a labelled value both).
pub fn tree_node_texts<S>(h: &egui_kittest::Harness<'_, S>) -> String {
    use egui_kittest::kittest::NodeT as _;
    h.root().children_recursive().map(|n| node_text(&n)).collect::<Vec<_>>().join("\n")
}

/// Every node with `role` whose label is exactly `label`, in tree order (creation order).
pub fn nodes<'t, S>(
    h: &'t egui_kittest::Harness<'_, S>,
    role: egui::accesskit::Role,
    label: &str,
) -> Vec<egui_kittest::Node<'t>> {
    use egui_kittest::kittest::NodeT as _;
    h.root()
        .children_recursive()
        .filter(|n| {
            let a = n.accesskit_node();
            a.role() == role && a.label().as_deref() == Some(label)
        })
        .collect()
}

/// Every button's label, in tree order (an unlabelled button reads as `""`).
pub fn button_labels<S>(h: &egui_kittest::Harness<'_, S>) -> Vec<String> {
    use egui_kittest::kittest::NodeT as _;
    h.root()
        .children_recursive()
        .filter(|n| n.accesskit_node().role() == egui::accesskit::Role::Button)
        .map(|n| n.accesskit_node().label().unwrap_or_default().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first frame installs and draws nothing; the second draws an icon, which on egui's
    /// default fonts would panic — so reaching the assertion proves the family is bound.
    #[test]
    fn the_first_frame_installs_the_type_and_the_next_draws_an_icon() {
        let ctx = egui::Context::default();
        let mut ready = Vec::new();
        for _ in 0..2 {
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                let ok = type_ready(ui.ctx());
                ready.push(ok);
                if ok {
                    ui.label(crate::icons::CLOSE);
                }
            });
            // epaint 0.36 panics on dropping texture deltas nobody applied.
            out.textures_delta.clear();
        }
        assert_eq!(ready, [false, true]);
    }

    /// The first frame installs the appearance and draws nothing; afterwards painters read it back.
    #[test]
    fn the_first_frame_installs_the_appearance_and_painters_read_it_back() {
        use crate::appearance::{Appearance, current};
        use crate::theme::ThemeId;
        let a = Appearance { theme: ThemeId::Dusk, ..Appearance::default() };
        let ctx = egui::Context::default();
        let mut ready = Vec::new();
        for _ in 0..2 {
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                ready.push(appearance_ready(ui.ctx(), &a));
            });
            out.textures_delta.clear();
        }
        assert_eq!(ready, [false, true]);
        assert_eq!(current(&ctx), a);
    }
}
