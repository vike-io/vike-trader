//! The general `[[value]]` table of `crates/vike-ui-theme/ui-theme.toml` (`tests/tokens_gen/values.rs` reads
//! it; `src/value.rs` holds what it generates).
//!
//! Three kinds of test, none of which names a row of the real table, because rows are added by many
//! hands in parallel and a test that listed them would be the first thing each of them broke:
//!
//! - the REAL table against the GENERATED registry, both directions — every row has its constant with
//!   the same kind, unit, doc and value, every entry has its row — and against the page and the CSS;
//! - the generator against PLANTED tables: each refusal it owes, one test apiece, and every kind
//!   written as the Rust, CSS and HTML it should be;
//! - the layout that keeps parallel edits merging: groups come out alphabetical whatever order the
//!   TOML lists them, and the first row of one group and the first row of another change lines that
//!   have an unchanged line between them, in the Rust, the CSS and the page.
//!
//! The drift gate — that the committed `value_tokens.rs`, `ui-theme.css` and `brand-book.html` are what
//! the generator writes — is `tests/brand_assets.rs`'s, with the other generated files.

use std::collections::BTreeSet;

use egui::Color32;
use vike_ui_theme::value;

mod tokens_gen;

use tokens_gen::values::{self, Data, Unit};

fn real_text() -> String {
    std::fs::read_to_string(tokens_gen::toml_path()).expect("ui-theme.toml reads")
}

// ---- the real table against the registry ---------------------------------------------------------

/// Whether the constant the app compiles holds the value the TOML row says.
fn same_value(compiled: &value::Data, toml: &Data) -> bool {
    match (compiled, toml) {
        (value::Data::F32(a), Data::F32(b)) => a.to_bits() == b.to_bits(),
        (value::Data::F64(a), Data::F64(b)) => a.to_bits() == b.to_bits(),
        (value::Data::U8(a), Data::U8(b)) => a == b,
        (value::Data::I8(a), Data::I8(b)) => a == b,
        (value::Data::U16(a), Data::U16(b)) => a == b,
        (value::Data::Vec2(a), Data::Vec2([w, h])) => {
            a.x.to_bits() == w.to_bits() && a.y.to_bits() == h.to_bits()
        }
        (value::Data::Colour(a), Data::Colour([r, g, b, al])) => {
            *a == Color32::from_rgba_unmultiplied(*r, *g, *b, *al)
        }
        _ => false,
    }
}

/// `parse` refuses a row under the wrong sentinel, a group with none and every other fault the planted
/// tests below plant; the real file must pass it.
#[test]
fn the_real_table_parses_and_every_row_sits_under_its_groups_sentinel() {
    values::parse(&real_text()).unwrap_or_else(|e| panic!("{e}"));
}

/// Every row of the TOML has its constant in the registry, with the same kind, unit, doc and value.
#[test]
fn every_toml_row_has_its_rust_constant_with_the_same_value() {
    let table = tokens_gen::Tokens::load();
    for row in &table.values.rows {
        let entry = value::find(&row.group, &row.name).unwrap_or_else(|| {
            panic!(
                "{}.{} is in ui-theme.toml but not in value_tokens.rs; regenerate (VIKE_REGEN_BRAND_ASSETS=1)",
                row.group, row.name
            )
        });
        let at = format!("{}.{}", row.group, row.name);
        assert_eq!(row.kind().key(), entry.kind().key(), "{at}: kind");
        assert_eq!(row.unit.map(Unit::key), entry.unit.map(value::Unit::key), "{at}: unit");
        assert_eq!(row.doc, entry.doc, "{at}: doc");
        assert!(
            same_value(&entry.value, &row.data),
            "{at}: toml {:?} but compiled {:?}",
            row.data,
            entry.value
        );
    }
}

/// ...and the other way: no constant outlives its row.
#[test]
fn every_registry_entry_has_a_toml_row() {
    let table = tokens_gen::Tokens::load();
    for entry in value::all() {
        let found = table.values.in_group(entry.group).any(|r| r.name == entry.name);
        assert!(
            found,
            "{}.{} is in value_tokens.rs but not in ui-theme.toml",
            entry.group, entry.name
        );
    }
}

/// The registry's groups are the sentinels' groups, alphabetical, each with exactly its rows.
#[test]
fn the_registry_holds_every_declared_group_alphabetically() {
    let table = tokens_gen::Tokens::load();
    let compiled: Vec<&str> = value::GROUPS.iter().map(|g| g.name).collect();
    let declared: Vec<&str> = table.values.groups.iter().map(String::as_str).collect();
    assert_eq!(
        compiled, declared,
        "value_tokens.rs's groups are not the sentinels of ui-theme.toml"
    );
    for g in value::GROUPS {
        assert_eq!(g.entries.len(), table.values.in_group(g.name).count(), "{}: row count", g.name);
    }
}

#[test]
fn no_group_and_name_pair_is_listed_twice() {
    let mut seen = BTreeSet::new();
    for e in value::all() {
        assert!(seen.insert((e.group, e.name)), "{}.{} is listed twice", e.group, e.name);
    }
}

/// The page shows every row, in its own group, with the value text and a drawing of the kind that
/// value is — read from the registry, so a row nobody listed here is covered too.
#[test]
fn the_brand_book_shows_every_value_row_drawn() {
    let table = tokens_gen::Tokens::load();
    let page = tokens_gen::book(&table);
    for entry in value::all() {
        let at = format!("{}.{}", entry.group, entry.name);
        let open = format!("<tr id=\"value-{}-{}\">", entry.group, entry.name);
        let start = page.find(&open).unwrap_or_else(|| panic!("{at} is not on the brand book"));
        let row_html = &page[start..start + page[start..].find("</tr>").expect("a row ends")];
        assert!(row_html.contains(&format!("<code>{}</code>", entry.name)), "{at}: its name");
        let row =
            table.values.in_group(entry.group).find(|r| r.name == entry.name).expect("its row");
        assert!(same_value(&entry.value, &row.data), "{at}: the page would draw a different value");
        let text = values::value_text(&row.data, row.unit);
        assert!(row_html.contains(&text), "{at}: its value `{text}` is not in {row_html}");
        // A length of nothing, or a ratio outside 0..=1, is drawn as a dash: there is no bar to see.
        let positive = match entry.value {
            value::Data::F32(v) => v > 0.0,
            value::Data::F64(v) => v > 0.0,
            value::Data::U8(v) => v > 0,
            value::Data::I8(v) => v > 0,
            value::Data::U16(v) => v > 0,
            _ => true,
        };
        let drawing = match (entry.value, entry.unit) {
            (value::Data::Colour(_), _) => Some("class=\"vsw\""),
            (value::Data::Vec2(_), _) => Some("class=\"vbox\""),
            (_, Some(value::Unit::Alpha)) => Some("class=\"wash\""),
            (value::Data::F32(v), Some(value::Unit::Ratio)) if (0.0..=1.0).contains(&v) => {
                Some("class=\"vmeter\"")
            }
            (value::Data::F64(v), Some(value::Unit::Ratio)) if (0.0..=1.0).contains(&v) => {
                Some("class=\"vmeter\"")
            }
            (_, Some(value::Unit::Px)) if positive => Some("class=\"vbar"),
            _ => None,
        };
        if let Some(class) = drawing {
            assert!(row_html.contains(class), "{at}: not drawn ({class}) in {row_html}");
        }
        assert!(
            page.contains(&format!("<h3 id=\"values-{}\">", entry.group)),
            "{at}: its group has no heading"
        );
    }
}

/// Every row is a CSS variable (or two, for a size) carrying the value the code compiles.
#[test]
fn every_value_row_is_a_css_variable() {
    let table = tokens_gen::Tokens::load();
    let css = tokens_gen::css(&table);
    for e in value::all() {
        let var = values::css_var(e.group, e.name);
        let want: Vec<String> = match (e.value, e.unit) {
            (value::Data::Colour(_), _) => {
                // Not read back from the compiled `Color32` (premultiplied, so a translucent colour
                // would not return its own bytes): the row's, which `same_value` ties to it.
                let row =
                    table.values.in_group(e.group).find(|r| r.name == e.name).expect("its row");
                let Data::Colour([r, g, b, a]) = row.data else {
                    panic!("{}.{}: not a colour", e.group, e.name)
                };
                let hex = if a == 255 {
                    format!("#{r:02x}{g:02x}{b:02x}")
                } else {
                    format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
                };
                vec![format!("{var}: {hex};")]
            }
            (value::Data::Vec2(v), _) => {
                vec![format!("{var}-w: {}px;", v.x), format!("{var}-h: {}px;", v.y)]
            }
            (value::Data::F32(v), Some(value::Unit::Px)) => vec![format!("{var}: {v}px;")],
            (value::Data::F32(v), _) => vec![format!("{var}: {v};")],
            (value::Data::F64(v), Some(value::Unit::Px)) => vec![format!("{var}: {v}px;")],
            (value::Data::F64(v), _) => vec![format!("{var}: {v};")],
            (value::Data::U8(v), Some(value::Unit::Px)) => vec![format!("{var}: {v}px;")],
            (value::Data::U8(v), _) => vec![format!("{var}: {v};")],
            (value::Data::I8(v), _) => vec![format!("{var}: {v}px;")],
            (value::Data::U16(v), Some(value::Unit::Px)) => vec![format!("{var}: {v}px;")],
            (value::Data::U16(v), _) => vec![format!("{var}: {v};")],
        };
        for line in want {
            assert!(
                css.contains(&format!("  {line}\n")),
                "{}.{}: ui-theme.css has no `{line}`",
                e.group,
                e.name
            );
        }
    }
}

// ---- the generator against planted tables --------------------------------------------------------

/// One `[[value]]` row. An empty `unit` writes none.
fn row_doc(group: &str, name: &str, kind: &str, unit: &str, value: &str, doc: &str) -> String {
    let unit = if unit.is_empty() { String::new() } else { format!("unit = \"{unit}\"\n") };
    format!(
        "[[value]]\ngroup = \"{group}\"\nname = \"{name}\"\nkind = \"{kind}\"\n{unit}value = {value}\ndoc = \"{doc}\"\n\n"
    )
}

fn row(group: &str, name: &str, kind: &str, unit: &str, value: &str) -> String {
    row_doc(group, name, kind, unit, value, &format!("What {name} is."))
}

/// A table whose sections are `(group, rows)`, in the order given, each under its own sentinel.
fn table(sections: &[(&str, Vec<String>)]) -> String {
    let mut s = String::from("[meta]\nname = \"planted\"\n\n");
    for (group, rows) in sections {
        s.push_str(&format!("{}\n\n", values::sentinel(group)));
        rows.iter().for_each(|r| s.push_str(r));
    }
    s
}

fn parsed(text: &str) -> values::Values {
    values::parse(text).unwrap_or_else(|e| panic!("a planted table was refused: {e}"))
}

/// The generator refuses `text`, and its message holds `needle`.
fn refused(text: &str, needle: &str) {
    match values::parse(text) {
        Ok(_) => panic!("the generator accepted a table it should refuse ({needle})"),
        Err(e) => assert!(e.contains(needle), "the message `{e}` does not say `{needle}`"),
    }
}

/// One row refused: `row` is the only row, under group `g`.
fn row_refused(rows: Vec<String>, needle: &str) {
    refused(&table(&[("g", rows)]), needle);
}

#[test]
fn a_duplicate_group_and_name_is_refused() {
    let rows = vec![row("g", "A", "u8", "px", "1"), row("g", "A", "u8", "px", "2")];
    row_refused(rows, "duplicate value `g.A` (rows #1 and #2)");
}

#[test]
fn the_same_name_in_two_groups_is_two_rows() {
    let t = table(&[
        ("a", vec![row("a", "X", "u8", "px", "1")]),
        ("b", vec![row("b", "X", "u8", "px", "2")]),
    ]);
    assert_eq!(parsed(&t).rows.len(), 2);
}

#[test]
fn an_unknown_kind_is_refused() {
    row_refused(
        vec![row("g", "A", "i32", "px", "1")],
        "`kind` \"i32\" is not one of f32, f64, u8, i8, u16, vec2, colour",
    );
}

#[test]
fn a_value_that_does_not_fit_u8_is_refused() {
    row_refused(vec![row("g", "A", "u8", "px", "256")], "`value` 256 does not fit u8 (0..=255)");
    row_refused(vec![row("g", "A", "u8", "px", "-1")], "`value` -1 does not fit u8");
}

#[test]
fn a_value_that_does_not_fit_i8_is_refused() {
    row_refused(vec![row("g", "A", "i8", "px", "128")], "does not fit i8 (-128..=127)");
    row_refused(vec![row("g", "A", "i8", "px", "-129")], "does not fit i8");
}

#[test]
fn a_value_that_does_not_fit_u16_is_refused() {
    row_refused(vec![row("g", "A", "u16", "px", "65536")], "does not fit u16 (0..=65535)");
}

#[test]
fn a_decimal_point_in_a_whole_kind_is_refused() {
    row_refused(
        vec![row("g", "A", "u8", "px", "3.0")],
        "is not a whole number: a u8 is written without a decimal point",
    );
}

#[test]
fn a_string_where_a_number_belongs_is_refused() {
    row_refused(
        vec![row("g", "A", "f32", "px", "\"wide\"")],
        "is not a finite number that fits f32",
    );
    row_refused(vec![row("g", "A", "u8", "px", "\"7\"")], "is not a whole number");
    // (an `f64` string is in `a_value_that_does_not_fit_f64_is_refused`)
}

#[test]
fn a_float_that_is_not_finite_or_does_not_fit_f32_is_refused() {
    row_refused(vec![row("g", "A", "f32", "px", "inf")], "is not a finite number that fits f32");
    row_refused(vec![row("g", "A", "f32", "px", "nan")], "is not a finite number that fits f32");
    row_refused(vec![row("g", "A", "f32", "px", "1e40")], "is not a finite number that fits f32");
}

/// An `f64` row is refused what does not fit an `f64` — and is NOT refused what only an `f32` cannot hold.
#[test]
fn a_value_that_does_not_fit_f64_is_refused() {
    row_refused(vec![row("g", "A", "f64", "ratio", "inf")], "is not a finite number that fits f64");
    row_refused(
        vec![row("g", "A", "f64", "ratio", "-inf")],
        "is not a finite number that fits f64",
    );
    row_refused(vec![row("g", "A", "f64", "ratio", "nan")], "is not a finite number that fits f64");
    row_refused(
        vec![row("g", "A", "f64", "ratio", "\"0.34\"")],
        "`value` \"0.34\" is not a finite number that fits f64",
    );
    row_refused(vec![row("g", "A", "f64", "px", "1e999")], "ui-theme.toml:");
    // 1e40 overflows an f32 (refused above) and is an ordinary f64.
    let kept = parsed(&table(&[("g", vec![row("g", "A", "f64", "px", "1e40")])]));
    assert_eq!(kept.rows[0].data, Data::F64(1e40));
}

/// An `f64` is never narrowed on the way: every digit written is the constant's.
#[test]
fn an_f64_row_keeps_every_digit_an_f32_would_lose() {
    let t = table(&[("g", vec![row("g", "A", "f64", "ratio", "0.1234567890123")])]);
    let v = parsed(&t);
    assert_eq!(v.rows[0].data, Data::F64(0.1234567890123));
    assert!(values::rust(&v).contains("    pub const A: f64 = 0.1234567890123;\n"));
    assert!(values::css(&v).contains("  --value-g-a: 0.1234567890123;\n"));
}

/// An `f64` carries the units an `f32` does (px, ratio, alpha) and no others.
#[test]
fn an_f64_carries_px_ratio_and_alpha() {
    for unit in ["px", "ratio", "alpha"] {
        parsed(&table(&[("g", vec![row("g", "A", "f64", unit, "1.5")])]));
    }
    row_refused(
        vec![row("g", "A", "f64", "count", "1")],
        "a f64 cannot be in `count`; its unit is px, ratio or alpha",
    );
}

#[test]
fn a_size_that_is_not_a_pair_of_numbers_is_refused() {
    row_refused(vec![row("g", "A", "vec2", "px", "[1.0]")], "is not a pair `[width, height]`");
    row_refused(vec![row("g", "A", "vec2", "px", "[1, 2, 3]")], "is not a pair `[width, height]`");
    row_refused(vec![row("g", "A", "vec2", "px", "3")], "is not a pair `[width, height]`");
    row_refused(vec![row("g", "A", "vec2", "px", "[1, \"2\"]")], "must hold two finite numbers");
}

#[test]
fn a_malformed_colour_is_refused() {
    for bad in ["\"#12345\"", "\"red\"", "\"#gg0000\"", "\"3ee08a\"", "\"#3ee08a1\"", "7"] {
        row_refused(vec![row("g", "A", "colour", "", bad)], "is not");
    }
    row_refused(
        vec![row("g", "A", "colour", "", "\"#12345\"")],
        "`value` \"#12345\" is not `#rrggbb` or `#rrggbbaa`",
    );
}

#[test]
fn a_scalar_without_a_unit_is_refused() {
    for kind in ["f32", "f64", "u8", "i8", "u16", "vec2"] {
        let v = if kind == "vec2" { "[1, 2]" } else { "1" };
        row_refused(vec![row("g", "A", kind, "", v)], "`unit` is missing");
    }
}

#[test]
fn a_unit_the_kind_cannot_carry_is_refused() {
    row_refused(
        vec![row("g", "A", "u8", "ratio", "1")],
        "a u8 cannot be in `ratio`; its unit is px, alpha or count",
    );
    row_refused(vec![row("g", "A", "f32", "count", "1")], "a f32 cannot be in `count`");
    row_refused(vec![row("g", "A", "i8", "alpha", "1")], "a i8 cannot be in `alpha`");
    row_refused(vec![row("g", "A", "vec2", "ratio", "[1, 2]")], "a vec2 cannot be in `ratio`");
    row_refused(vec![row("g", "A", "u16", "ratio", "1")], "a u16 cannot be in `ratio`");
}

#[test]
fn an_unknown_unit_is_refused() {
    row_refused(
        vec![row("g", "A", "f32", "em", "1.0")],
        "`unit` \"em\" is not one of px, ratio, alpha, count",
    );
}

#[test]
fn a_colour_with_a_unit_is_refused() {
    row_refused(vec![row("g", "A", "colour", "px", "\"#000000\"")], "a colour carries no `unit`");
}

#[test]
fn a_group_that_cannot_be_a_module_name_is_refused() {
    for bad in ["Trade", "1st", "data-manager", "type", "mod", "egui"] {
        let t = format!("{}\n\n{}", values::sentinel(bad), row(bad, "A", "u8", "px", "1"));
        refused(&t, "sentinel group");
    }
}

#[test]
fn a_row_whose_group_has_no_sentinel_is_refused() {
    let t = table(&[("a", vec![row("b", "A", "u8", "px", "1")])]);
    refused(&t, "group `b` has no sentinel; add `# ==== value: b ====`");
}

#[test]
fn a_row_under_another_groups_sentinel_is_refused() {
    let t = table(&[("a", vec![row("b", "A", "u8", "px", "1")]), ("b", vec![])]);
    refused(&t, "`b.A` sits under `# ==== value: a ====`; move it under `# ==== value: b ====`");
}

#[test]
fn a_row_above_every_sentinel_is_refused() {
    let t = format!("{}{}", row("a", "A", "u8", "px", "1"), table(&[("a", vec![])]));
    refused(&t, "sits under no group sentinel at all");
}

#[test]
fn a_name_that_cannot_be_a_constant_is_refused() {
    for bad in ["w_symbol", "1A", "A-B", "Mixed", "ENTRIES"] {
        row_refused(vec![row("g", bad, "u8", "px", "1")], "`name`");
    }
    row_refused(
        vec![row("g", "ENTRIES", "u8", "px", "1")],
        "`ENTRIES` is the constant each group's module keeps its rows in",
    );
    row_refused(vec![row("g", "w_symbol", "u8", "px", "1")], "`w_symbol` is not UPPER_SNAKE");
}

#[test]
fn a_key_the_row_does_not_have_is_refused() {
    let r = "[[value]]\ngroup = \"g\"\nname = \"A\"\nkind = \"u8\"\nunit = \"px\"\nvalue = 1\ndoc = \"d\"\ncolor = \"#000000\"\n\n";
    row_refused(vec![r.to_string()], "has an unknown key `color`");
}

#[test]
fn a_missing_field_is_refused() {
    let no_doc =
        "[[value]]\ngroup = \"g\"\nname = \"A\"\nkind = \"u8\"\nunit = \"px\"\nvalue = 1\n\n";
    row_refused(vec![no_doc.to_string()], "`doc` is missing or not a string");
    let no_name =
        "[[value]]\ngroup = \"g\"\nkind = \"u8\"\nunit = \"px\"\nvalue = 1\ndoc = \"d\"\n\n";
    row_refused(vec![no_name.to_string()], "`name` is missing or not a string");
    let no_value =
        "[[value]]\ngroup = \"g\"\nname = \"A\"\nkind = \"u8\"\nunit = \"px\"\ndoc = \"d\"\n\n";
    row_refused(vec![no_value.to_string()], "`value` is missing");
}

#[test]
fn a_doc_that_is_empty_or_runs_over_two_lines_is_refused() {
    row_refused(
        vec![row_doc("g", "A", "u8", "px", "1", "  ")],
        "`doc` is empty or runs over more than one line",
    );
    row_refused(
        vec![row_doc("g", "A", "u8", "px", "1", "one\\ntwo")],
        "`doc` is empty or runs over more than one line",
    );
}

#[test]
fn a_malformed_sentinel_is_refused() {
    for bad in [
        "# ==== value:trade ====",
        "# ==== value: trade ===",
        "  # ==== value: trade ====",
        "# ==== value ====",
    ] {
        refused(&format!("{bad}\n\n"), "is not a group sentinel");
    }
}

#[test]
fn a_group_with_two_sentinels_is_refused() {
    let t = table(&[("a", vec![]), ("a", vec![])]);
    refused(&t, "group `a` has two sentinels; a group has exactly one");
}

#[test]
fn a_sentinel_must_be_followed_by_a_blank_line() {
    refused(
        "# ==== value: a ====\n[[value]]\n",
        "the sentinel of `a` must be followed by a blank line",
    );
    refused("# ==== value: a ====", "the sentinel of `a` must be followed by a blank line");
}

#[test]
fn rows_written_inline_are_refused_rather_than_misplaced() {
    let t = format!(
        "{}\n\nvalue = [ {{ group = \"a\", name = \"A\", kind = \"u8\", unit = \"px\", value = 1, doc = \"d\" }} ]\n",
        values::sentinel("a")
    );
    refused(&t, "1 rows but 0 `[[value]]` headers: write each row as its own `[[value]]` table");
}

#[test]
fn a_table_that_is_not_toml_is_refused_with_toml_s_own_message() {
    refused(&format!("{}\n\n[[value]\n", values::sentinel("a")), "ui-theme.toml:");
}

#[test]
fn a_table_with_no_rows_is_a_table_of_empty_groups() {
    let v = parsed(&table(&[("b", vec![]), ("a", vec![])]));
    assert_eq!(v.groups, ["a", "b"]);
    assert!(v.rows.is_empty());
}

// ---- every kind, written -------------------------------------------------------------------------

/// One row of every kind, under group `kinds`.
fn every_kind() -> String {
    table(&[(
        "kinds",
        vec![
            row("kinds", "LENGTH", "f32", "px", "25"),
            row("kinds", "FACTOR", "f32", "ratio", "0.45"),
            row("kinds", "FLOOR", "f32", "alpha", "30.5"),
            row("kinds", "BAR", "f64", "ratio", "0.34"),
            row("kinds", "EXACT", "f64", "ratio", "0.1234567890123"),
            row("kinds", "REACH", "f64", "px", "12.5"),
            row("kinds", "SHADE", "f64", "alpha", "30.5"),
            row("kinds", "HINT", "u8", "alpha", "30"),
            row("kinds", "RADIUS", "u8", "px", "3"),
            row("kinds", "LIMIT", "u8", "count", "6"),
            row("kinds", "MARGIN", "i8", "px", "-3"),
            row("kinds", "WIDE", "u16", "px", "600"),
            row("kinds", "TICKS", "u16", "count", "12"),
            row("kinds", "SIZE", "vec2", "px", "[600, 560.5]"),
            row("kinds", "SMALL", "vec2", "px", "[24, 20]"),
            row("kinds", "INK", "colour", "", "\"#3EE08A\""),
            row("kinds", "WASHED", "colour", "", "\"#57a5ff5a\""),
        ],
    )])
}

#[test]
fn every_kind_is_written_as_a_rust_constant_a_const_can_hold() {
    let rust = values::rust(&parsed(&every_kind()));
    for want in [
        "    pub const LENGTH: f32 = 25.0;\n",
        "    pub const FACTOR: f32 = 0.45;\n",
        "    pub const FLOOR: f32 = 30.5;\n",
        "    pub const BAR: f64 = 0.34;\n",
        "    pub const EXACT: f64 = 0.1234567890123;\n",
        "    pub const REACH: f64 = 12.5;\n",
        "    pub const SHADE: f64 = 30.5;\n",
        "    pub const HINT: u8 = 30;\n",
        "    pub const MARGIN: i8 = -3;\n",
        "    pub const WIDE: u16 = 600;\n",
        "    pub const SIZE: egui::Vec2 = egui::vec2(600.0, 560.5);\n",
        "    pub const INK: egui::Color32 = egui::Color32::from_rgb(62, 224, 138);\n",
        "    pub const WASHED: egui::Color32 = egui::Color32::from_rgba_unmultiplied_const(87, 165, 255, 90);\n",
        "    /// What LENGTH is.\n",
        "        super::Entry { group: \"kinds\", name: \"LENGTH\", unit: Some(super::Unit::Px), value: super::Data::F32(LENGTH), doc: \"What LENGTH is.\" },\n",
        "        super::Entry { group: \"kinds\", name: \"BAR\", unit: Some(super::Unit::Ratio), value: super::Data::F64(BAR), doc: \"What BAR is.\" },\n",
        "        super::Entry { group: \"kinds\", name: \"REACH\", unit: Some(super::Unit::Px), value: super::Data::F64(REACH), doc: \"What REACH is.\" },\n",
        "        super::Entry { group: \"kinds\", name: \"HINT\", unit: Some(super::Unit::Alpha), value: super::Data::U8(HINT), doc: \"What HINT is.\" },\n",
        "        super::Entry { group: \"kinds\", name: \"LIMIT\", unit: Some(super::Unit::Count), value: super::Data::U8(LIMIT), doc: \"What LIMIT is.\" },\n",
        "        super::Entry { group: \"kinds\", name: \"MARGIN\", unit: Some(super::Unit::Px), value: super::Data::I8(MARGIN), doc: \"What MARGIN is.\" },\n",
        "        super::Entry { group: \"kinds\", name: \"WIDE\", unit: Some(super::Unit::Px), value: super::Data::U16(WIDE), doc: \"What WIDE is.\" },\n",
        "        super::Entry { group: \"kinds\", name: \"FACTOR\", unit: Some(super::Unit::Ratio), value: super::Data::F32(FACTOR), doc: \"What FACTOR is.\" },\n",
        "        super::Entry { group: \"kinds\", name: \"SIZE\", unit: Some(super::Unit::Px), value: super::Data::Vec2(SIZE), doc: \"What SIZE is.\" },\n",
        "        super::Entry { group: \"kinds\", name: \"INK\", unit: None, value: super::Data::Colour(INK), doc: \"What INK is.\" },\n",
        "    Group { name: \"kinds\", entries: kinds::ENTRIES },\n",
    ] {
        assert!(rust.contains(want), "the Rust has no `{want}` in:\n{rust}");
    }
}

#[test]
fn every_kind_is_written_as_css_variables() {
    let css = values::css(&parsed(&every_kind()));
    for want in [
        "  /* values: kinds */\n",
        "  --value-kinds-length: 25px;\n",
        "  --value-kinds-factor: 0.45;\n",
        "  --value-kinds-floor: 30.5;\n",
        "  --value-kinds-bar: 0.34;\n",
        "  --value-kinds-exact: 0.1234567890123;\n",
        "  --value-kinds-reach: 12.5px;\n",
        "  --value-kinds-shade: 30.5;\n",
        "  --value-kinds-hint: 30;\n",
        "  --value-kinds-radius: 3px;\n",
        "  --value-kinds-limit: 6;\n",
        "  --value-kinds-margin: -3px;\n",
        "  --value-kinds-wide: 600px;\n",
        "  --value-kinds-ticks: 12;\n",
        "  --value-kinds-size-w: 600px;\n",
        "  --value-kinds-size-h: 560.5px;\n",
        "  --value-kinds-ink: #3ee08a;\n",
        "  --value-kinds-washed: #57a5ff5a;\n",
    ] {
        assert!(css.contains(want), "the CSS has no `{want}` in:\n{css}");
    }
}

#[test]
fn every_kind_is_drawn_on_the_page() {
    let page = values::book(&parsed(&every_kind()));
    let row_of = |name: &str| {
        let start = page.find(&format!("<tr id=\"value-kinds-{name}\">")).expect("a row");
        page[start..start + page[start..].find("</tr>").expect("its end")].to_string()
    };
    assert!(page.contains("<h3 id=\"values-kinds\">Kinds</h3>"));
    // A length is a bar as long as it is; a very long one fades.
    assert!(
        row_of("LENGTH").contains("<td>25 px</td>")
            && row_of("LENGTH").contains("class=\"vbar\" style=\"width:25px\"")
    );
    assert!(row_of("WIDE").contains("class=\"vbar cut\" style=\"width:600px\""));
    // A ratio is a meter; an opacity a wash as strong as it is, of 255; a count only its number.
    assert!(
        row_of("FACTOR").contains("<td>0.45</td>")
            && row_of("FACTOR").contains("class=\"vmeter\"><i style=\"width:45.0%\"")
    );
    // An f64 is drawn as the same kind of f32 is — and its text is written from every digit it holds.
    assert!(
        row_of("BAR").contains("<td>0.34</td>")
            && row_of("BAR").contains("class=\"vmeter\"><i style=\"width:34.0%\"")
    );
    assert!(row_of("EXACT").contains("<td>0.1234567890123</td>"));
    assert!(
        row_of("REACH").contains("<td>12.5 px</td>")
            && row_of("REACH").contains("class=\"vbar\" style=\"width:12.5px\"")
    );
    assert!(
        row_of("SHADE").contains("<td>30.5 / 255</td>")
            && row_of("SHADE").contains("class=\"wash\"")
    );
    assert!(
        row_of("HINT").contains("<td>30 / 255</td>")
            && row_of("HINT").contains("class=\"wash\"")
            && row_of("HINT").contains("accent) 11.8%")
    );
    assert!(
        row_of("LIMIT").contains("<td>6</td>")
            && row_of("LIMIT").contains("<td class=\"drawn\">—</td>")
    );
    // A size is a rectangle: as it is when small, at a quarter when large.
    assert!(
        row_of("SMALL").contains("<td>24 × 20 px</td>")
            && row_of("SMALL").contains("style=\"width:24px;height:20px\"")
    );
    assert!(
        row_of("SIZE").contains("<td>600 × 560.5 px</td>")
            && row_of("SIZE").contains("style=\"width:150px;height:140.125px\"")
            && row_of("SIZE").contains("quarter size")
    );
    // A colour is a swatch of itself, with its hex; an alpha colour keeps its alpha.
    assert!(
        row_of("INK").contains("<code>#3ee08a</code>")
            && row_of("INK").contains("background:#3ee08a")
    );
    assert!(row_of("WASHED").contains("background:#57a5ff5a"));
}

#[test]
fn a_doc_with_brackets_cannot_become_a_broken_intra_doc_link() {
    let t = table(&[("g", vec![row_doc("g", "A", "u8", "px", "1", "see [`Other`] and <b>")])]);
    let v = parsed(&t);
    assert!(values::rust(&v).contains("    /// see \\[`Other`\\] and <b>\n"));
    // The page shows the doc as written, with the code span set in code and the angle brackets escaped.
    assert!(values::book(&v).contains("see [<code>Other</code>] and &lt;b&gt;"));
}

// ---- the layout that lets parallel edits merge ---------------------------------------------------

#[test]
fn groups_come_out_alphabetical_whatever_order_the_toml_lists_them() {
    let a = || ("a", vec![row("a", "X", "u8", "px", "1")]);
    let b = || ("b", vec![row("b", "Y", "f32", "px", "2.5"), row("b", "Z", "u8", "count", "3")]);
    let c = || ("c", vec![row("c", "W", "colour", "", "\"#000000\"")]);
    let forward = parsed(&table(&[a(), b(), c()]));
    let reverse = parsed(&table(&[c(), b(), a()]));
    let shuffled = parsed(&table(&[b(), c(), a()]));
    for other in [&reverse, &shuffled] {
        assert_eq!(values::rust(&forward), values::rust(other));
        assert_eq!(values::css(&forward), values::css(other));
        assert_eq!(values::book(&forward), values::book(other));
    }
    assert_eq!(forward.groups, ["a", "b", "c"]);
    // Within a group, the rows keep the order the TOML gives them.
    let names: Vec<&str> = forward.in_group("b").map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["Y", "Z"]);
}

#[test]
fn an_empty_group_keeps_a_frame_in_every_output() {
    let v = parsed(&table(&[("a", vec![]), ("b", vec![]), ("c", vec![])]));
    let (rust, css, page) = (values::rust(&v), values::css(&v), values::book(&v));
    for g in ["a", "b", "c"] {
        assert!(rust.contains(&format!("pub mod {g} {{\n")), "the Rust has no `pub mod {g}`");
        assert!(rust.contains(&format!("    Group {{ name: \"{g}\", entries: {g}::ENTRIES }},\n")));
        assert!(css.contains(&format!("  /* values: {g} */\n")));
        assert!(page.contains(&format!("<!-- values: {g} -->\n")));
    }
    // ...and an empty group draws nothing but its frame.
    assert!(!page.contains("<h3") && !page.contains("<table"));
}

/// The lines of `before` that `after` replaced, as `(first, one past the last)` in `before`'s numbering,
/// and what stands there in `after`.
fn change(before: &str, after: &str) -> (usize, usize, Vec<String>) {
    let (b, a): (Vec<&str>, Vec<&str>) = (before.lines().collect(), after.lines().collect());
    let prefix = b.iter().zip(&a).take_while(|(x, y)| x == y).count();
    let suffix =
        b[prefix..].iter().rev().zip(a[prefix..].iter().rev()).take_while(|(x, y)| x == y).count();
    let replacement = a[prefix..a.len() - suffix].iter().map(|l| l.to_string()).collect();
    (prefix, b.len() - suffix, replacement)
}

/// Applies two changes to the same `before` the way a three-way merge does, or says they touch.
fn merge(
    before: &str,
    first: (usize, usize, Vec<String>),
    second: (usize, usize, Vec<String>),
) -> Option<String> {
    let lines: Vec<&str> = before.lines().collect();
    // A conflict is a change that touches the other: no unchanged line between them.
    if first.1 >= second.0 {
        return None;
    }
    let mut out: Vec<String> = lines[..first.0].iter().map(|l| l.to_string()).collect();
    out.extend(first.2);
    out.extend(lines[first.1..second.0].iter().map(|l| l.to_string()));
    out.extend(second.2);
    out.extend(lines[second.1..].iter().map(|l| l.to_string()));
    Some(out.join("\n") + "\n")
}

/// The four texts a table makes — the TOML a person edits, the Rust, the CSS and the page — for a planted
/// table whose groups `a`, `b` and `c` have rows exactly when they are named in `filled`.
fn texts(filled: &[&str]) -> Vec<(&'static str, String)> {
    let rows = |g: &str| vec![row(g, "ONE", "f32", "px", "10"), row(g, "TWO", "u8", "alpha", "20")];
    let sections: Vec<(&str, Vec<String>)> = ["a", "b", "c"]
        .into_iter()
        .map(|g| (g, if filled.contains(&g) { rows(g) } else { vec![] }))
        .collect();
    let text = table(&sections);
    let v = parsed(&text);
    vec![
        ("toml", text),
        ("rust", values::rust(&v)),
        ("css", values::css(&v)),
        ("page", values::book(&v)),
    ]
}

/// Merges, in each of the four texts, the edit that makes `one` out of `before` with the edit that makes
/// `two` out of it, and requires that they do not touch and that the result is `both`.
fn assert_merges(
    before: &[(&str, String)],
    one: &[(&str, String)],
    two: &[(&str, String)],
    both: &[(&str, String)],
    what: &str,
) {
    for (((b, o), t), w) in before.iter().zip(one).zip(two).zip(both) {
        let name = b.0;
        let merged = merge(&b.1, change(&b.1, &o.1), change(&b.1, &t.1))
            .unwrap_or_else(|| panic!("{name}: {what}: the two edits change adjacent lines"));
        assert_eq!(merged, w.1, "{name}: {what}: the merge is not what a table with both would be");
    }
}

/// The first row of one group and the first row of another change lines with an unchanged line between
/// them, so a three-way merge takes both — and what it produces is what the generator writes for the
/// merged table. Checked for every pair of three groups (first, middle, last), in the TOML a person edits
/// and in each of the three outputs that carry the rows.
#[test]
fn the_first_rows_of_two_groups_merge_without_touching() {
    let empty = texts(&[]);
    for (first, second) in [("a", "b"), ("b", "c"), ("a", "c")] {
        let what = format!("first rows of `{first}` and `{second}`");
        let (one, two, both) = (texts(&[first]), texts(&[second]), texts(&[first, second]));
        assert_merges(&empty, &one, &two, &both, &what);
    }
}

/// The same once a group in between already has rows: the neighbours' first rows still merge.
#[test]
fn a_group_between_two_edits_that_already_has_rows_does_not_bring_them_together() {
    assert_merges(
        &texts(&["b"]),
        &texts(&["a", "b"]),
        &texts(&["b", "c"]),
        &texts(&["a", "b", "c"]),
        "first rows of `a` and `c` around a filled `b`",
    );
}
