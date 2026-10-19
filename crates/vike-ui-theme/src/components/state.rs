//! What a pane shows when it has no rows (spec §4.2): loading, empty and unreachable are THREE
//! renderings. A pane still asking must not look like a pane that got nothing back, and neither may
//! look like one that could not ask.
//!
//! What each of the three looks like is a row of `ui-theme.toml`'s `empty_pane` map
//! (`crate::maps::empty_pane`): `colour` is the spinner's or the icon's, `icon` the glyph, `text` the
//! colour of the pane's words, `word` the icon's accessible name. `Load::row` only ties a state to
//! its row.

use egui::{Response, RichText, Ui};

use super::Tokens;
use crate::maps::{self, MapRow};
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

impl Load<'_> {
    /// This state's row of the `empty_pane` map. Exhaustive: a state without a row does not compile, and
    /// `every_state_has_its_own_row_and_every_row_its_state` holds the other direction.
    fn row(self) -> &'static MapRow {
        match self {
            Load::Loading(_) => &maps::empty_pane::LOADING,
            Load::Empty(_) => &maps::empty_pane::EMPTY,
            Load::Unreachable(_) => &maps::empty_pane::UNREACHABLE,
        }
    }
}

pub fn view(ui: &mut Ui, load: Load<'_>) -> Response {
    let t = Tokens::of(ui.ctx());
    let big = t.text.px(TextRole::Heading);
    let row = load.row();
    ui.vertical_centered(|ui| {
        ui.add_space(2.0 * t.metrics.gap);
        let mark = row.colour.resolve(&t);
        let words =
            |s: &str| RichText::new(s).font(t.font(TextRole::Body)).color(row.text.resolve(&t));
        match load {
            Load::Loading(what) => {
                ui.add(egui::Spinner::new().size(big).color(mark));
                ui.label(words(what));
            }
            Load::Empty(what) | Load::Unreachable(what) => {
                if let Some(icon) = row.icon() {
                    ui.label(icon.rich().size(big).color(mark));
                }
                ui.label(words(what));
            }
        }
    })
    .response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::Appearance;
    use crate::components::Status;
    use crate::components::testing::{ctx_with, paint, texts};
    use crate::icons;
    use crate::theme::ThemeId;
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

    /// Which colours the three panes wear, pinned to what `view` painted before they moved to
    /// `ui-theme.toml`'s `empty_pane` map: the spinner and its words in the caption grey, the tray in the
    /// caption grey with its words in the secondary text, the cloud in the warning amber with its words in the
    /// primary text — on every theme. Changing a row of the table changes the pane, and this test is what says
    /// so by name.
    #[test]
    fn a_pane_wears_the_colours_the_empty_pane_map_gives_it() {
        for id in ThemeId::ALL {
            let ctx = ctx_with(&Appearance { theme: id, ..Appearance::default() });
            let t = Tokens::of(&ctx);
            let (text, text2, text3) = (t.theme.text, t.theme.text2, t.theme.text3);
            for (load, said, mark, words) in [
                (Load::Loading("asking"), "asking", text3, text3),
                (Load::Empty("nothing"), "nothing", text3, text2),
                (Load::Unreachable("down"), "down", Status::Warning.color(), text),
            ] {
                let row = load.row();
                assert_eq!(row.colour.resolve(&t), mark, "{id:?} {load:?}: the mark");
                assert_eq!(row.text.resolve(&t), words, "{id:?} {load:?}: the words");
                let drawn = texts(&paint(&ctx, |ui| {
                    view(ui, load);
                }));
                assert!(
                    drawn.contains(&(said.to_string(), words)),
                    "{id:?} {load:?}: the words are painted in {words:?}: {drawn:?}"
                );
                if let Some(icon) = row.icon() {
                    assert!(
                        drawn.contains(&(icon.accessible_label(""), mark)),
                        "{id:?} {load:?}: the icon is painted in {mark:?}: {drawn:?}"
                    );
                }
            }
        }
        assert_eq!(maps::empty_pane::LOADING.icon(), None, "a spinner is no icon of the registry");
        assert_eq!(maps::empty_pane::EMPTY.icon(), Some(icons::EMPTY));
        assert_eq!(maps::empty_pane::UNREACHABLE.icon(), Some(icons::UNREACHABLE));
    }

    /// The map is the pane's own: every state reads the row of its own name, no two share one, and no row of
    /// the `empty_pane` map is left without a state.
    #[test]
    fn every_state_has_its_own_row_and_every_row_its_state() {
        let all = [
            (Load::Loading(""), "LOADING"),
            (Load::Empty(""), "EMPTY"),
            (Load::Unreachable(""), "UNREACHABLE"),
        ];
        for (load, key) in all {
            assert_eq!(load.row().key, key, "{load:?} reads another row");
            assert_eq!(
                all.iter().filter(|(other, _)| other.row() == load.row()).count(),
                1,
                "{load:?}'s row is shared"
            );
        }
        for row in maps::empty_pane::ALL {
            assert!(
                all.iter().any(|(load, _)| load.row() == *row),
                "{} is no state of a pane",
                row.key
            );
        }
    }
}
