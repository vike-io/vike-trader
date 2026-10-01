//! What a pane shows when it has no rows (spec §4.2): loading, empty and unreachable are THREE
//! renderings. A pane still asking must not look like a pane that got nothing back, and neither may
//! look like one that could not ask.

use egui::{Response, RichText, Ui};

use super::{Status, Tokens};
use crate::icons;
use crate::type_scale::TextRole;

/// Why a pane has no rows, and the sentence it says about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Load<'a> {
    /// Still asking: a spinner.
    Loading(&'a str),
    /// Asked and got nothing: the empty tray, in the caption grey.
    Empty(&'a str),
    /// Could not ask: the unreachable cloud, in the warning colour.
    Unreachable(&'a str),
}

pub fn view(ui: &mut Ui, load: Load<'_>) -> Response {
    let t = Tokens::of(ui.ctx());
    let big = t.text.px(TextRole::Heading);
    ui.vertical_centered(|ui| {
        ui.add_space(2.0 * t.metrics.gap);
        let words = |s: &str, c| RichText::new(s).font(t.font(TextRole::Body)).color(c);
        match load {
            Load::Loading(what) => {
                ui.add(egui::Spinner::new().size(big).color(t.theme.text3));
                ui.label(words(what, t.theme.text3));
            }
            Load::Empty(what) => {
                ui.label(icons::EMPTY.rich().size(big).color(t.theme.text3));
                ui.label(words(what, t.theme.text2));
            }
            Load::Unreachable(why) => {
                ui.label(icons::UNREACHABLE.rich().size(big).color(Status::Warning.color()));
                ui.label(words(why, t.theme.text));
            }
        }
    })
    .response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::Appearance;
    use crate::components::testing::{ctx_with, paint, texts};
    use egui::Shape;

    /// The three states draw different things, not only different words (spec §4.2).
    #[test]
    fn loading_empty_and_unreachable_render_differently() {
        let ctx = ctx_with(&Appearance::default());
        let t = Tokens::of(&ctx);
        let draw = |l: Load<'_>| {
            paint(&ctx, |ui| {
                view(ui, l);
            })
        };
        let (loading, empty, unreachable) =
            (draw(Load::Loading("…")), draw(Load::Empty("…")), draw(Load::Unreachable("…")));
        let tray = icons::EMPTY.accessible_label("");
        let cloud = icons::UNREACHABLE.accessible_label("");
        assert!(loading.iter().any(|s| matches!(s, Shape::Path(_))), "loading is a spinner");
        assert!(!texts(&loading).iter().any(|(s, _)| *s == tray || *s == cloud));
        assert!(texts(&empty).contains(&(tray, t.theme.text3)));
        assert!(texts(&unreachable).contains(&(cloud, Status::Warning.color())));
    }
}
