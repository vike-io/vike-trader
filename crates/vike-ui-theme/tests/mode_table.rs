//! The `[[mode]]` table of `crates/vike-ui-theme/ui-theme.toml`: which colour ROLE each part of an account
//! mode's chip wears (`tests/tokens_gen/modes.rs` reads it; `src/mode_tokens.rs` holds what it generates;
//! `src/roles.rs` resolves a role against the live theme).
//!
//! Three kinds of test, none of which names a mode of the real table, because the modes are the TABLE's and a
//! test that listed them would be a second list:
//!
//! - the REAL table against the GENERATED registry (`roles::modes::ALL`), both directions, and against the
//!   kit's `Mode` enum and the brand book — every comparison iterates a registry, never a hand list;
//! - the page against the kit: the colour the book writes for a role on a theme is the colour
//!   `ColourRole::resolve` gives it, for every role, theme and mode;
//! - the generator against PLANTED tables: each refusal it owes, one test apiece.
//!
//! The drift gate — that the committed `mode_tokens.rs` and `brand-book.html` are what the generator writes —
//! is `tests/brand_assets.rs`'s, with the other generated files. The chip's own tests (which role a mode
//! wears, every mode has a row and every row a mode) are `src/components/chip.rs`'s.

use egui::Color32;
use vike_ui_theme::appearance::Appearance;
use vike_ui_theme::components::Tokens;
use vike_ui_theme::components::chip::Mode;
use vike_ui_theme::roles::{ColourRole, MarkShape, modes};
use vike_ui_theme::theme::ThemeId;

mod tokens_gen;

use tokens_gen::modes as gen_modes;

fn real_text() -> String {
    std::fs::read_to_string(tokens_gen::toml_path()).expect("ui-theme.toml reads")
}

/// `#rrggbb` as the page writes a colour, or `transparent` for none.
fn css(c: Color32) -> String {
    if c == Color32::TRANSPARENT {
        "transparent".to_string()
    } else {
        format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
    }
}

fn tokens(theme: &str) -> Tokens {
    let id = ThemeId::from_key(theme).unwrap_or_else(|| panic!("{theme} is not a theme"));
    Tokens::from_appearance(&Appearance { theme: id, ..Appearance::default() })
}

// ---- the real table against the registry, the kit and the page -------------------------------------

#[test]
fn the_real_table_parses() {
    gen_modes::parse(&real_text()).unwrap_or_else(|e| panic!("{e}"));
}

/// Every row of the TOML has its constant in the registry, in the same order, with the same roles, mark
/// and doc — the registry is what the app compiles — and no constant outlives its row.
#[test]
fn every_toml_row_has_its_constant_with_the_same_roles() {
    let rows = tokens_gen::Tokens::load().modes;
    let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    let compiled: Vec<&str> = modes::ALL.iter().map(|r| r.name).collect();
    assert_eq!(
        compiled, names,
        "mode_tokens.rs does not list the TOML's modes in its order; regenerate (VIKE_REGEN_BRAND_ASSETS=1)"
    );
    for row in &rows {
        let entry = modes::ALL.iter().find(|e| e.name == row.name).expect("its constant");
        let at = &row.name;
        assert_eq!(entry.fill, row.fill, "{at}: fill");
        assert_eq!(entry.stroke, row.stroke, "{at}: stroke");
        assert_eq!(entry.text, row.text, "{at}: text");
        assert_eq!(entry.mark, row.mark, "{at}: mark");
        assert_eq!(entry.mark_colour, row.mark_colour, "{at}: mark_colour");
        assert_eq!(entry.doc, row.doc, "{at}: doc");
    }
}

/// The kit draws a mode for every TOML row and a row backs every mode it draws, by the label the chip
/// prints — read from the TOML itself, so this holds before the registry is regenerated.
#[test]
fn every_toml_row_is_a_mode_of_the_kit_and_every_mode_has_a_toml_row() {
    let rows = tokens_gen::Tokens::load().modes;
    for row in &rows {
        assert!(
            Mode::ALL.iter().any(|m| m.label() == row.name),
            "[[mode]] {} is no mode of the kit: add the variant, or delete the row",
            row.name
        );
    }
    for m in Mode::ALL {
        assert!(
            rows.iter().any(|r| r.name == m.label()),
            "{m:?} has no [[mode]] row in ui-theme.toml"
        );
    }
}

/// The colour the page writes for a role on a theme is the colour the kit paints it, for every role and
/// theme — the generator reads the TOML and the kit reads the compiled constants, so this is what says the
/// brand book shows what the app draws.
#[test]
fn the_page_writes_each_role_as_the_kit_resolves_it() {
    let t = tokens_gen::Tokens::load();
    for id in tokens_gen::THEMES {
        let kit = tokens(id);
        for role in ColourRole::ALL {
            assert_eq!(
                gen_modes::role_css(&t, id, role),
                css(role.resolve(&kit)),
                "{id}: the page's `{}` is not the kit's",
                role.key()
            );
        }
    }
}

/// The page draws every mode of the registry in every theme: its row is there, it names each role and the
/// mark's shape, and each theme's chip is written in the colours the kit resolves for that theme.
#[test]
fn the_brand_book_draws_every_mode_in_every_theme() {
    let page = tokens_gen::book(&tokens_gen::Tokens::load());
    for role in ColourRole::ALL {
        assert!(page.contains(&format!("<code>{}</code>", role.key())), "role {}", role.key());
    }
    for row in modes::ALL {
        let open = format!("<tr id=\"mode-{}\">", row.name);
        let start =
            page.find(&open).unwrap_or_else(|| panic!("{} is not on the brand book", row.name));
        let html = &page[start..start + page[start..].find("</tr>").expect("a row ends")];
        for (part, role) in [("fill", row.fill), ("stroke", row.stroke), ("text", row.text)] {
            let line = format!("<div>{part} <code>{}</code></div>", role.key());
            assert!(html.contains(&line), "{}: `{line}` is not in {html}", row.name);
        }
        let mark = format!(
            "<div>mark <code>{}</code> <code>{}</code></div>",
            row.mark.key(),
            row.mark_colour.key()
        );
        assert!(html.contains(&mark), "{}: `{mark}` is not in {html}", row.name);
        for id in tokens_gen::THEMES {
            let kit = tokens(id);
            let chip = format!(
                "<span class=\"mchip\" style=\"background:{};border-color:{};color:{}\">{}</span>",
                css(row.fill.resolve(&kit)),
                css(row.stroke.resolve(&kit)),
                css(row.text.resolve(&kit)),
                row.name
            );
            assert!(html.contains(&chip), "{} on {id}: `{chip}` is not in {html}", row.name);
            let mark = match row.mark {
                MarkShape::Filled => format!(
                    "<i class=\"mmark filled\" style=\"background:{}\"></i>",
                    css(row.mark_colour.resolve(&kit))
                ),
                MarkShape::Ring => format!(
                    "<i class=\"mmark ring\" style=\"border-color:{}\"></i>",
                    css(row.mark_colour.resolve(&kit))
                ),
            };
            assert!(html.contains(&mark), "{} on {id}: `{mark}` is not in {html}", row.name);
        }
    }
}

// ---- the generator against planted tables ----------------------------------------------------------

/// A well-formed row, with the fields a test then spoils.
struct Planted {
    name: &'static str,
    fill: &'static str,
    stroke: &'static str,
    text: &'static str,
    mark: &'static str,
    mark_colour: &'static str,
}

const GOOD: Planted = Planted {
    name: "A",
    fill: "accent",
    stroke: "none",
    text: "on_fill",
    mark: "filled",
    mark_colour: "accent",
};

impl Planted {
    /// The row as TOML text, omitting `skip` (a key) when given.
    fn toml_without(&self, skip: Option<&str>) -> String {
        let lines = [
            ("name", self.name),
            ("fill", self.fill),
            ("stroke", self.stroke),
            ("text", self.text),
            ("mark", self.mark),
            ("mark_colour", self.mark_colour),
        ];
        let mut s = String::from("[[mode]]\n");
        for (key, value) in lines {
            if Some(key) != skip {
                s.push_str(&format!("{key} = \"{value}\"\n"));
            }
        }
        if skip != Some("doc") {
            s.push_str(&format!("doc = \"What {} is.\"\n", self.name));
        }
        s.push('\n');
        s
    }

    fn toml(&self) -> String {
        self.toml_without(None)
    }
}

fn table(rows: &[String]) -> String {
    let mut s = String::from("[meta]\nname = \"planted\"\n\n");
    rows.iter().for_each(|r| s.push_str(r));
    s
}

/// The generator refuses `text`, and its message holds `needle`.
fn refused(text: &str, needle: &str) {
    match gen_modes::parse(text) {
        Ok(_) => panic!("the generator accepted a table it should refuse ({needle})"),
        Err(e) => assert!(e.contains(needle), "the message `{e}` does not say `{needle}`"),
    }
}

fn one_refused(row: Planted, needle: &str) {
    refused(&table(&[row.toml()]), needle);
}

#[test]
fn a_planted_good_row_parses_and_is_written_as_rust() {
    let second = Planted { name: "B", fill: "none", stroke: "border", mark: "ring", ..GOOD };
    let rows = gen_modes::parse(&table(&[GOOD.toml(), second.toml()])).expect("a good table");
    assert_eq!(rows.len(), 2);
    assert_eq!((rows[0].fill, rows[0].stroke), (ColourRole::Accent, ColourRole::None));
    assert_eq!(rows[1].mark, MarkShape::Ring);
    let rust = gen_modes::rust(&rows);
    assert!(
        rust.contains(
            "    pub const A: ModeRow = ModeRow { name: \"A\", fill: ColourRole::Accent, stroke: ColourRole::None, text: ColourRole::OnFill, mark: MarkShape::Filled, mark_colour: ColourRole::Accent, doc: \"What A is.\" };\n"
        ),
        "{rust}"
    );
    assert!(rust.contains("    pub const ALL: &[ModeRow] = &[A, B];\n"), "{rust}");
}

/// A word that is not a role is refused in every part that takes a role, and the message prints the
/// vocabulary — `ColourRole::ALL` itself, so the list the generator shows cannot be a stale copy.
#[test]
fn an_unknown_role_is_refused_and_the_roles_are_listed() {
    let cases = [
        (Planted { fill: "mauve", ..GOOD }, "`fill` \"mauve\" is not a colour role"),
        (Planted { stroke: "mauve", ..GOOD }, "`stroke` \"mauve\" is not a colour role"),
        (Planted { text: "mauve", ..GOOD }, "`text` \"mauve\" is not a colour role"),
        (Planted { mark_colour: "mauve", ..GOOD }, "`mark_colour` \"mauve\" is not a colour role"),
        // a hex is not a role: a part is named by what it is FOR
        (Planted { fill: "#3ee08a", ..GOOD }, "`fill` \"#3ee08a\" is not a colour role"),
    ];
    for (row, needle) in cases {
        let Err(e) = gen_modes::parse(&table(&[row.toml()])) else {
            panic!("the generator accepted a bad role ({needle})");
        };
        assert!(e.contains(needle), "the message `{e}` does not say `{needle}`");
        assert!(e.contains("[[mode]] #1 `A`"), "the message `{e}` does not name the row");
        for role in ColourRole::ALL {
            assert!(e.contains(role.key()), "the message `{e}` omits the role {}", role.key());
        }
    }
}

#[test]
fn a_duplicate_mode_is_refused() {
    let rows = [GOOD.toml(), Planted { fill: "none", ..GOOD }.toml()];
    refused(&table(&rows), "duplicate mode `A` (rows #1 and #2)");
}

/// Every field is required: a row that leaves one out is refused, naming the row and the field.
#[test]
fn a_missing_field_is_refused() {
    for field in ["name", "fill", "stroke", "text", "mark", "mark_colour", "doc"] {
        let text = table(&[GOOD.toml_without(Some(field))]);
        refused(&text, &format!("`{field}` is missing or not a string"));
    }
}

#[test]
fn a_key_the_row_does_not_have_is_refused() {
    let text = table(&[format!("{}\ncolour = \"#000000\"\n\n", GOOD.toml().trim_end())]);
    refused(&text, "has an unknown key `colour`");
}

/// `none` is a role for a fill and a stroke, and for nothing else: a label or a mark in no colour is not
/// drawn, so the generator refuses it rather than the app painting nothing.
#[test]
fn none_is_no_colour_for_a_label_or_a_mark() {
    one_refused(Planted { text: "none", ..GOOD }, "`text` cannot be `none`");
    one_refused(Planted { mark_colour: "none", ..GOOD }, "`mark_colour` cannot be `none`");
    let outline_only = Planted { fill: "none", stroke: "border", ..GOOD };
    gen_modes::parse(&table(&[outline_only.toml()])).expect("a fill of none is an outlined chip");
    let neither = Planted { fill: "none", stroke: "none", ..GOOD };
    gen_modes::parse(&table(&[neither.toml()])).expect("a chip may be its label alone");
}

#[test]
fn an_unknown_mark_shape_is_refused_and_the_shapes_are_listed() {
    let Err(e) = gen_modes::parse(&table(&[Planted { mark: "square", ..GOOD }.toml()])) else {
        panic!("the generator accepted a mark shape nobody draws");
    };
    assert!(e.contains("`mark` \"square\" is not one of"), "{e}");
    for shape in MarkShape::ALL {
        assert!(e.contains(shape.key()), "{e} omits {}", shape.key());
    }
}

#[test]
fn a_name_that_cannot_be_a_constant_is_refused() {
    for bad in ["live", "1A", "A-B", "Mixed", "ALL"] {
        one_refused(Planted { name: bad, ..GOOD }, "`name`");
    }
    one_refused(Planted { name: "ALL", ..GOOD }, "`ALL` is the registry");
    one_refused(Planted { name: "live", ..GOOD }, "`live` is not UPPER_SNAKE");
}

#[test]
fn a_doc_that_is_empty_or_runs_over_two_lines_is_refused() {
    for doc in ["  ", "one\\ntwo"] {
        let text = table(&[format!(
            "[[mode]]\nname = \"A\"\nfill = \"none\"\nstroke = \"border\"\ntext = \"text2\"\nmark = \"ring\"\nmark_colour = \"text3\"\ndoc = \"{doc}\"\n\n"
        )]);
        refused(&text, "`doc` is empty or runs over more than one line");
    }
}

#[test]
fn a_table_with_no_modes_is_refused() {
    refused("[meta]\nname = \"planted\"\n", "[[mode]] is missing");
    refused("mode = []\n", "[[mode]] is missing");
    refused("mode = 3\n", "`mode` must be an array of tables");
    refused("[[mode]\n", "ui-theme.toml:");
}
