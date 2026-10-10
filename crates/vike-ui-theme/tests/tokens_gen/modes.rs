//! The `[[mode]]` table of `ui-theme.toml`: where an account trades (LIVE, DEMO, PAPER) and which colour
//! ROLE each part of its chip wears. One row each: `name` (the label the chip prints and the Rust constant
//! that carries the row), `fill` and `stroke` (a role, or `none`), `text` (a role), `mark` (`filled` or `ring`:
//! how the venue chip draws the mode) with `mark_colour` (a role), and `doc`.
//!
//! A part is named by a ROLE (`accent`, `info`, `text2`...), never by a hex, so the row says "DEMO is outlined
//! in the information blue" and the colour itself stays where it is defined: the theme, or the status table.
//! The role is resolved at run time against the installed appearance by `vike_ui_theme::roles`.
//!
//! What it writes, from the parsed table:
//!
//! - `src/mode_tokens.rs` — `pub mod modes { pub const LIVE: ModeRow = …; pub const ALL: &[ModeRow] = … }`,
//!   which `components::chip::Mode` reads for the chip and the venue chip's mark;
//! - the "Account modes" section of `assets/brand/brand-book.html`: each mode's chip and mark drawn in each
//!   theme, beside the role names.
//!
//! # The vocabulary is not copied here
//!
//! The roles and the mark shapes are `vike_ui_theme::roles::{ColourRole, MarkShape}`, which this module
//! LINKS: a word is validated by `ColourRole::from_key`, and the refusals and the page print
//! `ColourRole::ALL`. A list kept in this file would be a second copy for a test to compare.
//!
//! Every refusal is a `Result::Err` naming the row, so a bad table stops the generator
//! (see [`super::Tokens::load`]) and `tests/mode_table.rs` can plant one and read the message.

use std::fmt::Write as _;

use toml::{Table, Value};
use vike_ui_theme::components::ON_FILL;
use vike_ui_theme::roles::{ColourRole, MarkShape};

use super::values::{doc_comment, string};
use super::{RUST_HEADER, THEMES, Tokens, colour, esc, hex_of, md_code};

/// The keys a row may carry.
const ROW_KEYS: [&str; 7] = ["name", "fill", "stroke", "text", "mark", "mark_colour", "doc"];

/// The constant the generated module keeps its registry in: a mode may not take its name.
const RESERVED_NAME: &str = "ALL";

/// One `[[mode]]` row, checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub name: String,
    pub fill: ColourRole,
    pub stroke: ColourRole,
    pub text: ColourRole,
    pub mark: MarkShape,
    pub mark_colour: ColourRole,
    pub doc: String,
}

/// Why `name` cannot be a mode's constant, if it cannot.
fn name_problem(name: &str) -> Option<String> {
    let upper = name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    if !upper {
        return Some(format!(
            "`{name}` is not UPPER_SNAKE (A-Z, 0-9 and `_`, starting with a letter): it becomes a Rust constant"
        ));
    }
    if name == RESERVED_NAME {
        return Some(format!(
            "`{RESERVED_NAME}` is the registry the generated module keeps its rows in"
        ));
    }
    None
}

/// The role a part is written as. `none` is a role for a fill or a stroke and for nothing else: a label or
/// a mark in no colour is not drawn.
fn role(t: &Table, key: &str, at: &str, none_ok: bool) -> Result<ColourRole, String> {
    let word = string(t, key, at)?;
    let Some(role) = ColourRole::from_key(word) else {
        let all: Vec<&str> = ColourRole::ALL.iter().map(|r| r.key()).collect();
        return Err(format!(
            "ui-theme.toml: {at}: `{key}` {word:?} is not a colour role; the roles are {}",
            all.join(", ")
        ));
    };
    if role == ColourRole::None && !none_ok {
        return Err(format!(
            "ui-theme.toml: {at}: `{key}` cannot be `none`: a {} in no colour is not drawn",
            if key == "text" { "label" } else { "mark" }
        ));
    }
    Ok(role)
}

/// One `[[mode]]` table, checked. `n` is its place in the file (1-based), for the message.
fn row(n: usize, t: &Table) -> Result<Row, String> {
    let at = format!("[[mode]] #{n}");
    if let Some(key) = t.keys().find(|k| !ROW_KEYS.contains(&k.as_str())) {
        return Err(format!(
            "ui-theme.toml: {at} has an unknown key `{key}` (a row has {})",
            ROW_KEYS.join(", ")
        ));
    }
    let name = string(t, "name", &at)?;
    let at = format!("{at} `{name}`");
    if let Some(problem) = name_problem(name) {
        return Err(format!("ui-theme.toml: {at}: `name` {problem}"));
    }
    let fill = role(t, "fill", &at, true)?;
    let stroke = role(t, "stroke", &at, true)?;
    let text = role(t, "text", &at, false)?;
    let shape = string(t, "mark", &at)?;
    let mark = MarkShape::from_key(shape).ok_or_else(|| {
        let all: Vec<&str> = MarkShape::ALL.iter().map(|s| s.key()).collect();
        format!("ui-theme.toml: {at}: `mark` {shape:?} is not one of {}", all.join(", "))
    })?;
    let mark_colour = role(t, "mark_colour", &at, false)?;
    let doc = string(t, "doc", &at)?.trim();
    if doc.is_empty() || doc.contains(['\n', '\r']) {
        return Err(format!("ui-theme.toml: {at}: `doc` is empty or runs over more than one line"));
    }
    Ok(Row { name: name.to_string(), fill, stroke, text, mark, mark_colour, doc: doc.to_string() })
}

/// Parses `ui-theme.toml`'s `[[mode]]` table out of its text, or says exactly what is wrong.
pub fn parse(text: &str) -> Result<Vec<Row>, String> {
    let table: Table = text.parse().map_err(|e| format!("ui-theme.toml: {e}"))?;
    let tables = match table.get("mode") {
        Some(Value::Array(a)) if !a.is_empty() => a,
        None | Some(Value::Array(_)) => {
            return Err("ui-theme.toml: [[mode]] is missing: the chip has no row to read".into());
        }
        Some(_) => {
            return Err("ui-theme.toml: `mode` must be an array of tables, `[[mode]]`".into());
        }
    };
    let mut rows: Vec<Row> = Vec::with_capacity(tables.len());
    for (i, t) in tables.iter().enumerate() {
        let t = t
            .as_table()
            .ok_or_else(|| format!("ui-theme.toml: [[mode]] #{} is not a table", i + 1))?;
        let r = row(i + 1, t)?;
        if let Some(first) = rows.iter().position(|p| p.name == r.name) {
            return Err(format!(
                "ui-theme.toml: duplicate mode `{}` (rows #{} and #{})",
                r.name,
                first + 1,
                i + 1
            ));
        }
        rows.push(r);
    }
    Ok(rows)
}

// ---- the Rust ----------------------------------------------------------------------------------

/// `src/mode_tokens.rs`: one `ModeRow` constant per row, in `pub mod modes`, then `ALL`.
pub fn rust(rows: &[Row]) -> String {
    let mut s = String::from(RUST_HEADER);
    s.push_str(
        "/// Every account mode: `ui-theme.toml`'s `[[mode]]` rows, one constant each, and `ALL`, the registry the brand book and the tests iterate.\npub mod modes {\n    use super::{ColourRole, MarkShape, ModeRow};\n\n",
    );
    for r in rows {
        let _ = writeln!(s, "    /// {}", doc_comment(&r.doc));
        let _ = writeln!(
            s,
            "    pub const {}: ModeRow = ModeRow {{ name: {:?}, fill: ColourRole::{:?}, stroke: ColourRole::{:?}, text: ColourRole::{:?}, mark: MarkShape::{:?}, mark_colour: ColourRole::{:?}, doc: {:?} }};",
            r.name, r.name, r.fill, r.stroke, r.text, r.mark, r.mark_colour, r.doc
        );
    }
    let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    let _ = writeln!(
        s,
        "\n    /// Every row, in the order the TOML lists them.\n    pub const ALL: &[ModeRow] = &[{}];\n}}",
        names.join(", ")
    );
    s
}

// ---- the page ----------------------------------------------------------------------------------

/// The colour `role` is on `theme` (a key of `THEMES`), as the page writes it: `#rrggbb`, or `transparent`
/// for `none`. Read from the TOML, never from the kit, so a test can hold it equal to
/// `ColourRole::resolve`: that comparison is what says the page shows the colours the app paints.
pub fn role_css(t: &Tokens, theme: &str, role: ColourRole) -> String {
    match role {
        // A theme role is named by the theme field it reads.
        ColourRole::Accent
        | ColourRole::Border
        | ColourRole::Text
        | ColourRole::Text2
        | ColourRole::Text3
        | ColourRole::Bg
        | ColourRole::Surface
        | ColourRole::Card
        | ColourRole::Hover
        | ColourRole::TextUi
        | ColourRole::GradTop
        | ColourRole::AnalysisLine => {
            hex_of(colour(t.table(&["themes", theme]), role.key(), &format!("themes.{theme}")))
        }
        // A market role is named by the market set's field, read from the DEFAULT set (Classic): the set is the
        // trader's choice and is not part of the page's theme switch.
        ColourRole::Up | ColourRole::Down | ColourRole::UpText | ColourRole::DownText => {
            hex_of(colour(t.table(&["market", "classic"]), role.key(), "market.classic"))
        }
        // A status role is named by the `[[status]]` row it reads, upper-cased.
        ColourRole::Ok
        | ColourRole::Warning
        | ColourRole::Error
        | ColourRole::Info
        | ColourRole::Muted => {
            let name = role.key().to_uppercase();
            let entry = t
                .array_of_tables("status")
                .into_iter()
                .find(|e| e.get("name").and_then(Value::as_str) == Some(name.as_str()))
                .unwrap_or_else(|| {
                    panic!(
                        "ui-theme.toml: [[status]] has no {name}, which is the `{}` role",
                        role.key()
                    )
                });
            hex_of(colour(entry, "hex", &format!("status.{name}")))
        }
        ColourRole::OnFill => {
            let [r, g, b, _] = ON_FILL.to_array();
            hex_of([r, g, b])
        }
        ColourRole::None => "transparent".to_string(),
    }
}

/// The "Account modes" section's body: the vocabulary, then a table with a row per mode and a column per
/// theme, each cell the mode's chip and mark drawn from its roles on that theme's background.
pub fn book(t: &Tokens) -> String {
    let mut s = String::new();
    let words: Vec<String> =
        ColourRole::ALL.iter().map(|r| format!("<code>{}</code>", r.key())).collect();
    let _ = writeln!(
        s,
        "<p class=\"lead\">The roles: {}. A role is read against the theme the trader chose, so one row is a different colour on each theme below; a status role is the same on all four.</p>",
        words.join(" ")
    );
    s.push_str("<table class=\"modes\"><thead><tr><th>Mode</th><th>Roles</th>");
    for id in THEMES {
        let label = string(t.table(&["themes", id]), "label", &format!("themes.{id}"))
            .unwrap_or_else(|e| panic!("{e}"));
        let _ = write!(s, "<th>{}</th>", esc(label));
    }
    s.push_str("</tr></thead><tbody>\n");
    for r in &t.modes {
        let _ = write!(
            s,
            "<tr id=\"mode-{0}\"><td><b>{0}</b></td><td><div>fill <code>{1}</code></div><div>stroke <code>{2}</code></div><div>text <code>{3}</code></div><div>mark <code>{4}</code> <code>{5}</code></div><small>{6}</small></td>",
            r.name,
            r.fill.key(),
            r.stroke.key(),
            r.text.key(),
            r.mark.key(),
            r.mark_colour.key(),
            md_code(&r.doc)
        );
        for id in THEMES {
            let c = |role: ColourRole| role_css(t, id, role);
            let mark = match r.mark {
                MarkShape::Filled => format!(
                    "<i class=\"mmark filled\" style=\"background:{}\"></i>",
                    c(r.mark_colour)
                ),
                MarkShape::Ring => {
                    format!(
                        "<i class=\"mmark ring\" style=\"border-color:{}\"></i>",
                        c(r.mark_colour)
                    )
                }
            };
            let _ = write!(
                s,
                "<td style=\"background:{}\"><span class=\"mchip\" style=\"background:{};border-color:{};color:{}\">{}</span>{mark}</td>",
                c(ColourRole::Bg),
                c(r.fill),
                c(r.stroke),
                c(r.text),
                r.name
            );
        }
        s.push_str("</tr>\n");
    }
    s.push_str("</tbody></table>\n");
    s
}
