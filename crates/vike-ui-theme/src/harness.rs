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

/// Whether the app's type is bound on `ctx` for THIS frame — installing it, and answering `false`,
/// the first time a context asks. Call it at the top of a harness's UI closure and draw nothing
/// until it answers `true`.
pub fn type_ready(ctx: &egui::Context) -> bool {
    let id = egui::Id::new("vike_ui_theme::harness::type_ready");
    if ctx.data(|d| d.get_temp::<bool>(id)).unwrap_or(false) {
        return true;
    }
    crate::appearance::install_type(ctx, crate::type_scale::TextSize::Small);
    ctx.data_mut(|d| d.insert_temp(id, true));
    ctx.request_repaint();
    false
}

/// [`type_ready`]'s twin for a harness that tests a surface UNDER an appearance — a theme, a market
/// set, a density, a text size. The first time a context asks, it installs `a` with
/// [`crate::appearance::install`]: the bundled faces, and everything `apply` sets, kept where
/// `appearance::current` reads it. It answers `false` that frame and requests a repaint; every later
/// call answers `true`.
pub fn appearance_ready(ctx: &egui::Context, a: &crate::appearance::Appearance) -> bool {
    let id = egui::Id::new("vike_ui_theme::harness::appearance_ready");
    if ctx.data(|d| d.get_temp::<bool>(id)).unwrap_or(false) {
        return true;
    }
    crate::appearance::install(ctx, a);
    ctx.data_mut(|d| d.insert_temp(id, true));
    ctx.request_repaint();
    false
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
