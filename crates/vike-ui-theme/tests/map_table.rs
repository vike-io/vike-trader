//! The general `[[map]]` table of `crates/vike-ui-theme/ui-theme.toml` (`tests/tokens_gen/maps.rs` reads it;
//! `src/maps.rs` holds what it generates): which colour, word or icon each state or kind of a mapping uses.
//!
//! Three kinds of test, none of which names a row of the real table, because rows are added by many hands in
//! parallel and a test that listed them would be the first thing each of them broke:
//!
//! - the REAL table against the GENERATED registry (`maps::all_maps()`), both directions — every row has its
//!   constant with the same roles, word, icon, count, flag and doc, every constant has its row — and against the
//!   page, which shows every row with each role as the swatch the kit resolves in Graphite;
//! - the generator against PLANTED tables: each refusal it owes, one test apiece, and every field written as
//!   the Rust and the HTML it should be;
//! - the layout that keeps parallel edits merging: maps come out alphabetical whatever order the TOML lists
//!   them, and the first row of one map and the first row of another change lines that have an unchanged line
//!   between them, in the TOML, the Rust and the page.
//!
//! The drift gate — that the committed `map_tokens.rs` and `brand-book.html` are what the generator writes —
//! is `tests/brand_assets.rs`'s, with the other generated files. The registry's own invariants (every row
//! resolves on every theme, every icon exists) are `src/maps.rs`'s; a consumer's (an enum with a row for each
//! variant) is the consumer's.

use egui::Color32;
use vike_ui_theme::appearance::Appearance;
use vike_ui_theme::components::Tokens;
use vike_ui_theme::icons;
use vike_ui_theme::maps;
use vike_ui_theme::roles::ColourRole;
use vike_ui_theme::theme::ThemeId;

mod tokens_gen;

use tokens_gen::maps as gen_maps;

fn real_text() -> String {
    std::fs::read_to_string(tokens_gen::toml_path()).expect("ui-theme.toml reads")
}

/// `#rrggbb` as the page writes a colour.
fn css(c: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

fn graphite() -> Tokens {
    Tokens::from_appearance(&Appearance { theme: ThemeId::Graphite, ..Appearance::default() })
}

/// The cell the page writes for a role: a swatch of what the kit resolves it to in Graphite, or a dash.
fn swatch(role: ColourRole) -> String {
    if role == ColourRole::None {
        return "<td>—</td>".to_string();
    }
    format!(
        "<td><i class=\"msw\" style=\"background:{}\"></i><code>{}</code></td>",
        css(role.resolve(&graphite())),
        role.key()
    )
}

/// The page's HTML for one row: from its `<tr>` to its `</tr>`.
fn row_html<'a>(page: &'a str, map: &str, key: &str) -> &'a str {
    let open = format!("<tr id=\"map-{map}-{key}\">");
    let start = page.find(&open).unwrap_or_else(|| panic!("{map}.{key} is not on the brand book"));
    &page[start..start + page[start..].find("</tr>").expect("a row ends")]
}

// ---- the real table against the registry and the page ----------------------------------------------

/// `parse` refuses a row under the wrong sentinel, a map with none and every other fault the planted tests
/// below plant; the real file must pass it.
#[test]
fn the_real_table_parses_and_every_row_sits_under_its_maps_sentinel() {
    gen_maps::parse(&real_text()).unwrap_or_else(|e| panic!("{e}"));
}

/// Every row of the TOML has its constant in the registry with the same roles, word, icon, count, flag and
/// doc — the registry is what the app compiles.
#[test]
fn every_toml_row_has_its_rust_constant_with_the_same_fields() {
    let table = tokens_gen::Tokens::load();
    for row in &table.maps.rows {
        let at = format!("{}.{}", row.map, row.key);
        let entry = maps::find(&row.map, &row.key).unwrap_or_else(|| {
            panic!("{at} is in ui-theme.toml but not in map_tokens.rs; regenerate (VIKE_REGEN_BRAND_ASSETS=1)")
        });
        assert_eq!(entry.colour, row.colour, "{at}: colour");
        assert_eq!(entry.fill, row.fill, "{at}: fill");
        assert_eq!(entry.stroke, row.stroke, "{at}: stroke");
        assert_eq!(entry.text, row.text, "{at}: text");
        assert_eq!(entry.word, row.word.as_deref(), "{at}: word");
        assert_eq!(entry.icon, row.icon.as_deref(), "{at}: icon");
        assert_eq!(entry.count, row.count, "{at}: count");
        assert_eq!(entry.flag, row.flag, "{at}: flag");
        assert_eq!(entry.doc, row.doc, "{at}: doc");
    }
}

/// ...and the other way: no constant outlives its row.
#[test]
fn every_registry_row_has_a_toml_row() {
    let table = tokens_gen::Tokens::load();
    for m in maps::all_maps() {
        for r in m.rows {
            let found = table.maps.in_map(m.name).any(|t| t.key == r.key);
            assert!(found, "{}.{} is in map_tokens.rs but not in ui-theme.toml", m.name, r.key);
        }
    }
}

/// The registry's maps are the sentinels' maps, alphabetical, each with exactly its rows in the TOML's order.
#[test]
fn the_registry_holds_every_declared_map_alphabetically_with_its_rows_in_order() {
    let table = tokens_gen::Tokens::load();
    let compiled: Vec<&str> = maps::all_maps().iter().map(|m| m.name).collect();
    let declared: Vec<&str> = table.maps.maps.iter().map(String::as_str).collect();
    assert_eq!(compiled, declared, "map_tokens.rs's maps are not the sentinels of ui-theme.toml");
    for m in maps::all_maps() {
        let keys: Vec<&str> = m.rows.iter().map(|r| r.key).collect();
        let written: Vec<&str> = table.maps.in_map(m.name).map(|r| r.key.as_str()).collect();
        assert_eq!(keys, written, "{}: the rows are not the TOML's, in its order", m.name);
    }
}

/// Every icon a real row names is a key of the kit's registry.
#[test]
fn every_icon_a_real_row_names_is_in_the_icon_registry() {
    for row in &tokens_gen::Tokens::load().maps.rows {
        if let Some(key) = &row.icon {
            assert!(
                icons::ALL.iter().any(|(k, _)| *k == key.as_str()),
                "{}.{}: `{key}` is not a key of icons::ALL",
                row.map,
                row.key
            );
        }
    }
}

/// The page shows every row of the registry in its own map: its key, each role as the swatch of the colour
/// the kit resolves in Graphite, its word, its icon as the real glyph beside its key, its count, its flag and
/// its doc — read from the registry, so a row nobody listed here is covered too.
#[test]
fn the_brand_book_shows_every_map_row() {
    let page = tokens_gen::book(&tokens_gen::Tokens::load());
    for m in maps::all_maps() {
        assert!(page.contains(&format!("<!-- maps: {} -->\n", m.name)), "{}: no frame", m.name);
        if !m.rows.is_empty() {
            assert!(page.contains(&format!("<h3 id=\"maps-{}\">", m.name)), "{}: heading", m.name);
        }
        for r in m.rows {
            let at = format!("{}.{}", m.name, r.key);
            let html = row_html(&page, m.name, r.key);
            assert!(html.contains(&format!("<td><code>{}</code></td>", r.key)), "{at}: its key");
            for (part, role) in
                [("colour", r.colour), ("fill", r.fill), ("stroke", r.stroke), ("text", r.text)]
            {
                let cell = swatch(role);
                assert!(html.contains(&cell), "{at}: {part} `{cell}` is not in {html}");
            }
            if let Some(w) = r.word
                && !w.contains(['&', '<', '>', '"'])
            {
                assert!(
                    html.contains(&format!("<td>{w}</td>")),
                    "{at}: word {w:?} is not in {html}"
                );
            }
            if let Some(key) = r.icon {
                let glyph = gen_maps::glyph(key).expect("the icon is in the registry");
                let cell = format!("<span class=\"mico\">{glyph}</span><code>{key}</code>");
                assert!(html.contains(&cell), "{at}: icon `{cell}` is not in {html}");
            }
            if let Some(c) = r.count {
                assert!(html.contains(&format!("<td>{c}</td>")), "{at}: count {c}");
            }
            if let Some(f) = r.flag {
                assert!(html.contains(&format!("<td>{f}</td>")), "{at}: flag {f}");
            }
        }
    }
}

// ---- the generator against planted tables ----------------------------------------------------------

/// One `[[map]]` row: `map` and `key`, then each `(field, TOML literal)`, then a `doc` unless one is given.
fn row(map: &str, key: &str, fields: &[(&str, &str)]) -> String {
    let mut s = format!("[[map]]\nmap = \"{map}\"\nkey = \"{key}\"\n");
    for (field, literal) in fields {
        s.push_str(&format!("{field} = {literal}\n"));
    }
    if !fields.iter().any(|(f, _)| *f == "doc") {
        s.push_str(&format!("doc = \"What {key} is.\"\n"));
    }
    s.push('\n');
    s
}

/// A table whose sections are `(map, rows)`, in the order given, each under its own sentinel.
fn table(sections: &[(&str, Vec<String>)]) -> String {
    let mut s = String::from("[meta]\nname = \"planted\"\n\n");
    for (map, rows) in sections {
        s.push_str(&format!("{}\n\n", gen_maps::sentinel(map)));
        rows.iter().for_each(|r| s.push_str(r));
    }
    s
}

fn parsed(text: &str) -> gen_maps::Maps {
    gen_maps::parse(text).unwrap_or_else(|e| panic!("a planted table was refused: {e}"))
}

/// The generator refuses `text`, and its message holds `needle`.
fn refused(text: &str, needle: &str) {
    match gen_maps::parse(text) {
        Ok(_) => panic!("the generator accepted a table it should refuse ({needle})"),
        Err(e) => assert!(e.contains(needle), "the message `{e}` does not say `{needle}`"),
    }
}

/// One row refused: it is the only row, under map `m`.
fn row_refused(r: String, needle: &str) {
    refused(&table(&[("m", vec![r])]), needle);
}

#[test]
fn a_duplicate_map_and_key_is_refused() {
    let rows = vec![row("m", "A", &[]), row("m", "A", &[("colour", "\"accent\"")])];
    refused(&table(&[("m", rows)]), "duplicate map row `m.A` (rows #1 and #2)");
}

#[test]
fn the_same_key_in_two_maps_is_two_rows() {
    let t = table(&[("a", vec![row("a", "X", &[])]), ("b", vec![row("b", "X", &[])])]);
    assert_eq!(parsed(&t).rows.len(), 2);
}

#[test]
fn a_field_the_row_does_not_have_is_refused() {
    row_refused(row("m", "A", &[("colur", "\"accent\"")]), "has an unknown key `colur`");
    row_refused(row("m", "A", &[("hex", "\"#000000\"")]), "has an unknown key `hex`");
}

/// A word that is not a role is refused in every part that takes a role, and the message prints the
/// vocabulary — `ColourRole::ALL` itself, so the list the generator shows cannot be a stale copy.
#[test]
fn an_unknown_role_is_refused_and_the_roles_are_listed() {
    for part in ["colour", "fill", "stroke", "text"] {
        // a hex is not a role: a part is named by what it is FOR
        for bad in ["mauve", "#3ee08a"] {
            let literal = format!("\"{bad}\"");
            let text = table(&[("m", vec![row("m", "A", &[(part, literal.as_str())])])]);
            let Err(e) = gen_maps::parse(&text) else {
                panic!("the generator accepted `{part} = {bad}`");
            };
            assert!(e.contains(&format!("`{part}` \"{bad}\" is not a colour role")), "{e}");
            assert!(e.contains("[[map]] #1 `m.A`"), "the message `{e}` does not name the row");
            for role in ColourRole::ALL {
                assert!(e.contains(role.key()), "the message `{e}` omits the role {}", role.key());
            }
        }
    }
}

#[test]
fn a_role_that_is_not_a_string_is_refused() {
    row_refused(row("m", "A", &[("colour", "3")]), "`colour` is not a string");
    row_refused(row("m", "A", &[("fill", "true")]), "`fill` is not a string");
}

/// `none` is a role for every part of a map row: a map says what a state looks like, and "nothing" is a look.
#[test]
fn none_is_a_role_for_every_part() {
    let all = [
        ("colour", "\"none\""),
        ("fill", "\"none\""),
        ("stroke", "\"none\""),
        ("text", "\"none\""),
    ];
    let rows = parsed(&table(&[("m", vec![row("m", "A", &all)])])).rows;
    let r = &rows[0];
    assert_eq!(
        (r.colour, r.fill, r.stroke, r.text),
        (ColourRole::None, ColourRole::None, ColourRole::None, ColourRole::None)
    );
}

/// An icon that is not a key of the kit's registry is refused, the message lists every key, and the Phosphor
/// name (what `Icon::name` returns) is not the key.
#[test]
fn an_unknown_icon_is_refused_and_the_registry_keys_are_listed() {
    for bad in ["plug", "not_an_icon", "connections", ""] {
        let literal = format!("\"{bad}\"");
        let text = table(&[("m", vec![row("m", "A", &[("icon", literal.as_str())])])]);
        let Err(e) = gen_maps::parse(&text) else {
            panic!("the generator accepted `icon = {bad:?}`");
        };
        assert!(e.contains(&format!("`icon` {bad:?} is not a key of the icon registry")), "{e}");
        assert!(e.contains("[[map]] #1 `m.A`"), "the message `{e}` does not name the row");
        for (key, _) in icons::ALL {
            assert!(e.contains(*key), "the message `{e}` omits the key {key}");
        }
    }
    let (key, icon) = icons::ALL[0];
    let phosphor = format!("\"{}\"", icon.name());
    assert_ne!(key, icon.name(), "the test needs an icon whose key and Phosphor name differ");
    row_refused(row("m", "A", &[("icon", phosphor.as_str())]), "is not a key of the icon registry");
    row_refused(row("m", "A", &[("icon", "4")]), "`icon` is not a string");
}

#[test]
fn every_key_of_the_icon_registry_is_accepted() {
    for (key, _) in icons::ALL {
        let literal = format!("\"{key}\"");
        let rows =
            parsed(&table(&[("m", vec![row("m", "A", &[("icon", literal.as_str())])])])).rows;
        assert_eq!(rows[0].icon.as_deref(), Some(*key));
    }
}

#[test]
fn a_key_that_cannot_be_a_constant_is_refused() {
    for bad in ["live", "1A", "A-B", "Mixed", "", "A B"] {
        row_refused(row("m", bad, &[]), "`key`");
    }
    row_refused(row("m", "live", &[]), "`live` is not UPPER_SNAKE");
    row_refused(row("m", "ALL", &[]), "`ALL` is the constant each map's module keeps its rows in");
}

#[test]
fn a_map_that_cannot_be_a_module_is_refused() {
    for (bad, why) in [
        ("Bad", "is not snake_case"),
        ("1st", "is not snake_case"),
        ("two-words", "is not snake_case"),
        ("match", "is a Rust keyword"),
        ("super", "is a Rust keyword"),
        ("tests", "would shadow an item `maps.rs` keeps beside the generated modules"),
        ("all_maps", "would shadow an item `maps.rs` keeps beside the generated modules"),
    ] {
        refused(&format!("# ==== map: {bad} ====\n\n"), &format!("sentinel map `{bad}` {why}"));
    }
    // ...and the same words on a row, though a sentinel is what stops them first
    row_refused(row("Bad", "A", &[]), "`map` `Bad` is not snake_case");
}

#[test]
fn a_map_with_no_sentinel_is_refused() {
    row_refused(
        row("ghost", "A", &[]),
        "map `ghost` has no sentinel; add `# ==== map: ghost ====`",
    );
}

#[test]
fn a_row_under_another_maps_sentinel_is_refused() {
    let t = table(&[("a", vec![row("b", "X", &[])]), ("b", vec![])]);
    refused(&t, "`b.X` sits under `# ==== map: a ====`; move it under `# ==== map: b ====`");
}

#[test]
fn a_row_before_any_sentinel_is_refused() {
    let t = format!("{}{}", row("a", "X", &[]), table(&[("a", vec![])]));
    refused(&t, "`a.X` sits under no map sentinel at all");
}

/// Every field a row must carry is required: a row that leaves one out is refused, naming the row and the
/// field.
#[test]
fn a_missing_map_key_or_doc_is_refused() {
    let t = |omit: &str| {
        let lines = [("map", "\"m\""), ("key", "\"A\""), ("doc", "\"What A is.\"")];
        let mut s = String::from("[[map]]\n");
        for (k, v) in lines {
            if k != omit {
                s.push_str(&format!("{k} = {v}\n"));
            }
        }
        table(&[("m", vec![s])])
    };
    for field in ["map", "key", "doc"] {
        refused(&t(field), &format!("`{field}` is missing or not a string"));
    }
}

#[test]
fn a_doc_that_is_empty_or_runs_over_two_lines_is_refused() {
    for doc in ["\"  \"", "\"\"", "\"one\\ntwo\"", "\"one\\r\\ntwo\""] {
        row_refused(
            row("m", "A", &[("doc", doc)]),
            "`doc` is empty or runs over more than one line",
        );
    }
}

#[test]
fn a_word_that_is_empty_padded_or_over_two_lines_is_refused() {
    for word in ["\"\"", "\" Ready\"", "\"Ready \"", "\"two\\nlines\"", "\"  \""] {
        row_refused(
            row("m", "A", &[("word", word)]),
            "is empty, runs over more than one line, or has a space at either end",
        );
    }
    row_refused(row("m", "A", &[("word", "7")]), "`word` is not a string");
    let ok = parsed(&table(&[(
        "m",
        vec![row("m", "A", &[("word", "\"Ready · every order asks first\"")])],
    )]));
    assert_eq!(ok.rows[0].word.as_deref(), Some("Ready · every order asks first"));
}

#[test]
fn a_count_that_does_not_fit_u8_is_refused() {
    row_refused(row("m", "A", &[("count", "256")]), "`count` 256 does not fit u8 (0..=255)");
    row_refused(row("m", "A", &[("count", "-1")]), "`count` -1 does not fit u8 (0..=255)");
    row_refused(
        row("m", "A", &[("count", "3.0")]),
        "is not a whole number: a u8 is written without a decimal point",
    );
    row_refused(row("m", "A", &[("count", "\"3\"")]), "`count` \"3\" is not a whole number");
    let ok = parsed(&table(&[(
        "m",
        vec![row("m", "A", &[("count", "255")]), row("m", "B", &[("count", "0")])],
    )]));
    assert_eq!((ok.rows[0].count, ok.rows[1].count), (Some(255), Some(0)));
}

#[test]
fn a_flag_that_is_not_a_boolean_is_refused() {
    row_refused(row("m", "A", &[("flag", "\"yes\"")]), "`flag` \"yes\" is not `true` or `false`");
    row_refused(row("m", "A", &[("flag", "1")]), "`flag` 1 is not `true` or `false`");
    let ok = parsed(&table(&[(
        "m",
        vec![row("m", "A", &[("flag", "false")]), row("m", "B", &[("flag", "true")])],
    )]));
    assert_eq!((ok.rows[0].flag, ok.rows[1].flag), (Some(false), Some(true)));
}

#[test]
fn a_malformed_sentinel_is_refused() {
    for bad in [
        "# ==== map:trade ====",
        "# ==== map: trade ===",
        "  # ==== map: trade ====",
        "# ==== map ====",
        "# ==== map: ====",
    ] {
        refused(&format!("{bad}\n\n"), "is not a map sentinel");
    }
}

#[test]
fn a_map_with_two_sentinels_is_refused() {
    let t = table(&[("a", vec![]), ("a", vec![])]);
    refused(&t, "map `a` has two sentinels; a map has exactly one");
}

#[test]
fn a_sentinel_must_be_followed_by_a_blank_line() {
    refused(
        "# ==== map: a ====\n[[map]]\n",
        "the sentinel of `a` must be followed by a blank line",
    );
    refused("# ==== map: a ====", "the sentinel of `a` must be followed by a blank line");
}

#[test]
fn rows_written_inline_are_refused_rather_than_misplaced() {
    let t = format!(
        "{}\n\nmap = [ {{ map = \"a\", key = \"A\", doc = \"d\" }} ]\n",
        gen_maps::sentinel("a")
    );
    refused(&t, "1 rows but 0 `[[map]]` headers: write each row as its own `[[map]]` table");
}

#[test]
fn a_map_that_is_not_an_array_of_tables_is_refused() {
    refused("map = 3\n", "`map` must be an array of tables");
}

#[test]
fn a_table_that_is_not_toml_is_refused_with_toml_s_own_message() {
    refused(&format!("{}\n\n[[map]\n", gen_maps::sentinel("a")), "ui-theme.toml:");
}

#[test]
fn a_table_with_no_rows_is_a_table_of_empty_maps() {
    let v = parsed(&table(&[("b", vec![]), ("a", vec![])]));
    assert_eq!(v.maps, ["a", "b"]);
    assert!(v.rows.is_empty());
    assert_eq!(parsed("[meta]\nname = \"planted\"\n").maps, Vec::<String>::new());
}

// ---- every field, written --------------------------------------------------------------------------

/// A row of every field, and a bare row, under map `full`.
fn every_field() -> String {
    let (icon, _) = icons::ALL[0];
    let icon = format!("\"{icon}\"");
    table(&[(
        "full",
        vec![
            row(
                "full",
                "ALL_FIELDS",
                &[
                    ("colour", "\"accent\""),
                    ("fill", "\"none\""),
                    ("stroke", "\"border\""),
                    ("text", "\"on_fill\""),
                    ("word", "\"A \\\"quoted\\\" word & more\""),
                    ("icon", icon.as_str()),
                    ("count", "3"),
                    ("flag", "true"),
                    ("doc", "\"see [`Other`] and <b>\""),
                ],
            ),
            row("full", "BARE", &[]),
        ],
    )])
}

#[test]
fn every_field_is_written_as_a_rust_constant_and_a_registry_line() {
    let rust = gen_maps::rust(&parsed(&every_field()));
    let (icon, _) = icons::ALL[0];
    for want in [
        "/// Full: the `[[map]]` rows of `ui-theme.toml` whose `map` is `full`.\npub mod full {\n".to_string(),
        "    /// see \\[`Other`\\] and <b>\n".to_string(),
        format!("    pub const ALL_FIELDS: super::MapRow = super::MapRow {{ key: \"ALL_FIELDS\", colour: super::ColourRole::Accent, fill: super::ColourRole::None, stroke: super::ColourRole::Border, text: super::ColourRole::OnFill, word: Some(\"A \\\"quoted\\\" word & more\"), icon: Some(\"{icon}\"), count: Some(3), flag: Some(true), doc: \"see [`Other`] and <b>\" }};\n"),
        "    pub const BARE: super::MapRow = super::MapRow { key: \"BARE\", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: \"What BARE is.\" };\n".to_string(),
        "    pub const ALL: &[&super::MapRow] = &[&ALL_FIELDS, &BARE];\n".to_string(),
        "    Map { name: \"full\", rows: full::ALL },\n".to_string(),
    ] {
        assert!(rust.contains(&want), "the Rust has no `{want}` in:\n{rust}");
    }
}

#[test]
fn every_field_is_drawn_on_the_page() {
    let t = tokens_gen::Tokens::load();
    let page = gen_maps::book(&t, &parsed(&every_field()));
    let (icon, _) = icons::ALL[0];
    assert!(page.contains("<h3 id=\"maps-full\">Full</h3>"));
    let all = row_html(&page, "full", "ALL_FIELDS");
    let glyph = gen_maps::glyph(icon).expect("a registry icon has a glyph");
    for want in [
        "<td><code>ALL_FIELDS</code></td>".to_string(),
        swatch(ColourRole::Accent),
        swatch(ColourRole::Border),
        swatch(ColourRole::OnFill),
        "<td>A &quot;quoted&quot; word &amp; more</td>".to_string(),
        format!("<td><span class=\"mico\">{glyph}</span><code>{icon}</code></td>"),
        "<td>3</td>".to_string(),
        "<td>true</td>".to_string(),
        "see [<code>Other</code>] and &lt;b&gt;".to_string(),
    ] {
        assert!(all.contains(&want), "the row has no `{want}` in {all}");
    }
    // a part the row leaves out, and a field it does not carry, are dashes
    let bare = row_html(&page, "full", "BARE");
    assert_eq!(bare.matches("<td>—</td>").count(), 8, "{bare}");
    assert!(!bare.contains("msw") && !bare.contains("mico"), "{bare}");
}

/// A glyph is the registry's own codepoint: the character reference the page prints is the one the kit draws.
#[test]
fn a_glyph_is_the_character_the_kit_draws() {
    for (key, icon) in icons::ALL {
        let glyph = gen_maps::glyph(key).expect("a glyph");
        let c = icon.rich().text().chars().next().expect("an icon is one character");
        assert_eq!(glyph, format!("&#x{:X};", c as u32), "{key}");
    }
    assert_eq!(gen_maps::glyph("no_such_icon"), None);
}

#[test]
fn a_doc_with_brackets_cannot_become_a_broken_intra_doc_link() {
    let rust = gen_maps::rust(&parsed(&every_field()));
    assert!(rust.contains("    /// see \\[`Other`\\] and <b>\n"));
}

// ---- the layout that lets parallel edits merge -----------------------------------------------------

#[test]
fn maps_come_out_alphabetical_whatever_order_the_toml_lists_them() {
    let a = || ("a", vec![row("a", "X", &[("count", "1")])]);
    let b = || ("b", vec![row("b", "Y", &[]), row("b", "Z", &[("flag", "true")])]);
    let c = || ("c", vec![row("c", "W", &[("colour", "\"text2\"")])]);
    let t = tokens_gen::Tokens::load();
    let forward = parsed(&table(&[a(), b(), c()]));
    let reverse = parsed(&table(&[c(), b(), a()]));
    let shuffled = parsed(&table(&[b(), c(), a()]));
    for other in [&reverse, &shuffled] {
        assert_eq!(gen_maps::rust(&forward), gen_maps::rust(other));
        assert_eq!(gen_maps::book(&t, &forward), gen_maps::book(&t, other));
    }
    assert_eq!(forward.maps, ["a", "b", "c"]);
    // Within a map, the rows keep the order the TOML gives them.
    let keys: Vec<&str> = forward.in_map("b").map(|r| r.key.as_str()).collect();
    assert_eq!(keys, ["Y", "Z"]);
}

#[test]
fn an_empty_map_keeps_a_frame_in_every_output() {
    let t = tokens_gen::Tokens::load();
    let v = parsed(&table(&[("a", vec![]), ("b", vec![]), ("c", vec![])]));
    let (rust, page) = (gen_maps::rust(&v), gen_maps::book(&t, &v));
    for m in ["a", "b", "c"] {
        assert!(rust.contains(&format!("pub mod {m} {{\n")), "the Rust has no `pub mod {m}`");
        assert!(rust.contains(&format!("    Map {{ name: \"{m}\", rows: {m}::ALL }},\n")));
        assert!(page.contains(&format!("<!-- maps: {m} -->\n")));
    }
    assert!(rust.contains("    pub const ALL: &[&super::MapRow] = &[];\n}\n"));
    // ...and an empty map draws nothing but its frame.
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

/// The three texts a table makes — the TOML a person edits, the Rust and the page — for a planted table
/// whose maps `a`, `b` and `c` have rows exactly when they are named in `filled`.
fn texts(filled: &[&str]) -> Vec<(&'static str, String)> {
    let rows = |m: &str| {
        vec![row(m, "ONE", &[("colour", "\"accent\"")]), row(m, "TWO", &[("count", "2")])]
    };
    let sections: Vec<(&str, Vec<String>)> = ["a", "b", "c"]
        .into_iter()
        .map(|m| (m, if filled.contains(&m) { rows(m) } else { vec![] }))
        .collect();
    let text = table(&sections);
    let v = parsed(&text);
    let t = tokens_gen::Tokens::load();
    vec![("toml", text), ("rust", gen_maps::rust(&v)), ("page", gen_maps::book(&t, &v))]
}

/// Merges, in each of the three texts, the edit that makes `one` out of `before` with the edit that makes
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

/// The first row of one map and the first row of another change lines with an unchanged line between them,
/// so a three-way merge takes both — and what it produces is what the generator writes for the merged table.
/// Checked for every pair of three maps (first, middle, last), in the TOML a person edits and in each of the
/// two outputs that carry the rows.
#[test]
fn the_first_rows_of_two_maps_merge_without_touching() {
    let empty = texts(&[]);
    for (first, second) in [("a", "b"), ("b", "c"), ("a", "c")] {
        let what = format!("first rows of `{first}` and `{second}`");
        let (one, two, both) = (texts(&[first]), texts(&[second]), texts(&[first, second]));
        assert_merges(&empty, &one, &two, &both, &what);
    }
}

/// The same once a map in between already has rows: the neighbours' first rows still merge.
#[test]
fn a_map_between_two_edits_that_already_has_rows_does_not_bring_them_together() {
    assert_merges(
        &texts(&["b"]),
        &texts(&["a", "b"]),
        &texts(&["b", "c"]),
        &texts(&["a", "b", "c"]),
        "first rows of `a` and `c` around a filled `b`",
    );
}
