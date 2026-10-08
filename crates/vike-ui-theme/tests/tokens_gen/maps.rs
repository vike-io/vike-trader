//! The general `[[map]]` table of `ui-theme.toml`: every "which colour, word or icon does this state or kind
//! use" mapping that would otherwise be a `match` ladder in Rust (a connection's state, a button's kind, a
//! calendar event's importance...). One row per state or kind: `map` (the mapping it belongs to), `key` (the
//! state: the Rust constant that carries the row), then — each OPTIONAL — `colour`, `fill`, `stroke`, `text`
//! (a colour ROLE each), `word` (a label), `icon` (the key of an icon of the kit's registry), `count` (a whole
//! number 0..=255), `flag` (true or false), and `doc` (one line, required).
//!
//! A part is named by a ROLE (`accent`, `info`, `text2`...), never by a hex, exactly as the `[[mode]]` table does
//! it, and for the same reason: the colour stays where it is defined, and a role is resolved at run time against the
//! installed appearance (`vike_ui_theme::roles::ColourRole::resolve`).
//!
//! What it writes, from the parsed table:
//!
//! - `src/map_tokens.rs` — per map `pub mod <map> { pub const <KEY>: MapRow = …; pub const ALL: &[&MapRow] = … }`
//!   and `MAPS`, the registry the page and the tests iterate. `src/maps.rs` includes it, and a Rust enum binds
//!   each of its variants to a row with an exhaustive `match`, so a variant without a row does not compile;
//! - the "Maps" section of `assets/brand/brand-book.html`: a table per map, each role as a swatch resolved in
//!   Graphite, each icon as its real glyph.
//!
//! # The vocabulary is not copied here
//!
//! The roles are `vike_ui_theme::roles::ColourRole`, and the icons are the keys of `vike_ui_theme::icons::ALL`:
//! this module LINKS both. A word is validated by `ColourRole::from_key`, an icon by looking its key up in
//! `icons::ALL`, and the refusals print `ColourRole::ALL` and the icon keys themselves — a list kept in this
//! file would be a second copy for a test to compare.
//!
//! # Built so that parallel edits merge
//!
//! Many people add rows to this table at once, each to their own map. Two things make that conflict-free,
//! and both are gated by `tests/map_table.rs` (they are the `[[value]]` table's, applied to maps):
//!
//! 1. In the TOML, every map has ONE sentinel line (`# ==== map: connection ====`, then a blank line), and its rows
//!    sit under it. Two maps' edits are therefore different places in the file, never adjacent. A map is DECLARED
//!    by its sentinel: a row naming a map with none is refused, and so is a row that sits under another map's.
//! 2. Every output holds the maps in alphabetical order whatever order the TOML lists them, and gives EVERY
//!    declared map a frame of its own (a `pub mod`, a `MAPS` line, an HTML comment) even while it has no rows.
//!    A map's rows therefore appear between lines that never change.
//!
//! Every refusal is a `Result::Err` with a message naming the row, so a bad table stops the generator (see
//! [`super::Tokens::load`]) and `tests/map_table.rs` can plant a bad table and read the message.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use toml::{Table, Value};
use vike_ui_theme::icons;
use vike_ui_theme::roles::ColourRole;

use super::modes::role_css;
use super::values::{KEYWORDS, doc_comment, label, string};
use super::{RUST_HEADER, Tokens, esc, md_code};

/// The keys a row may carry.
const ROW_KEYS: [&str; 11] =
    ["map", "key", "colour", "fill", "stroke", "text", "word", "icon", "count", "flag", "doc"];

/// The constant a map's module holds besides its rows: a row may not take its name.
const RESERVED_KEY: &str = "ALL";

/// Names a map may not take, besides a Rust keyword: `all_maps` is the registry function and `tests` the
/// hand-written module's own test module, both of which sit beside the generated `pub mod`s.
const RESERVED_MAPS: [&str; 2] = ["all_maps", "tests"];

/// The theme the page draws every role in.
const PAGE_THEME: &str = "graphite";

/// One `[[map]]` row, checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub map: String,
    pub key: String,
    pub colour: ColourRole,
    pub fill: ColourRole,
    pub stroke: ColourRole,
    pub text: ColourRole,
    pub word: Option<String>,
    pub icon: Option<String>,
    pub count: Option<u8>,
    pub flag: Option<bool>,
    pub doc: String,
}

/// The parsed table: the declared maps, alphabetical, and the rows grouped under them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Maps {
    /// Every map a sentinel declares, in alphabetical order (a map with no row is still here).
    pub maps: Vec<String>,
    /// Every row, ordered by map (alphabetical) and, within a map, as the TOML lists them.
    pub rows: Vec<Row>,
}

impl Maps {
    pub fn in_map<'a>(&'a self, map: &'a str) -> impl Iterator<Item = &'a Row> {
        self.rows.iter().filter(move |r| r.map == map)
    }
}

// ---- the table ---------------------------------------------------------------------------------

/// The sentinel line that declares `map`, and above which no row of it may sit.
pub fn sentinel(map: &str) -> String {
    format!("# ==== map: {map} ====")
}

/// Why `map` cannot be a module name, if it cannot.
fn map_problem(map: &str) -> Option<String> {
    let snake = map.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && map.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !snake {
        return Some(format!(
            "`{map}` is not snake_case (a-z, 0-9 and `_`, starting with a letter)"
        ));
    }
    if KEYWORDS.contains(&map) {
        return Some(format!("`{map}` is a Rust keyword, so it cannot name a module"));
    }
    if RESERVED_MAPS.contains(&map) {
        return Some(format!(
            "`{map}` would shadow an item `maps.rs` keeps beside the generated modules"
        ));
    }
    None
}

/// Why `key` cannot be a constant's name, if it cannot.
fn key_problem(key: &str) -> Option<String> {
    let upper = key.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && key.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    if !upper {
        return Some(format!(
            "`{key}` is not UPPER_SNAKE (A-Z, 0-9 and `_`, starting with a letter): it becomes a Rust constant"
        ));
    }
    if key == RESERVED_KEY {
        return Some(format!(
            "`{RESERVED_KEY}` is the constant each map's module keeps its rows in"
        ));
    }
    None
}

/// The declared maps (file order) and, for each `[[map]]` header, the map whose sentinel is above it.
type Layout = (Vec<String>, Vec<Option<String>>);

/// Reads the sentinels out of the raw text (a comment is invisible to the TOML parser).
fn scan_layout(text: &str) -> Result<Layout, String> {
    let lines: Vec<&str> = text.lines().collect();
    let (mut maps, mut at_header): (Vec<String>, Vec<Option<String>>) = (Vec::new(), Vec::new());
    let mut current: Option<String> = None;
    for (i, raw) in lines.iter().enumerate() {
        let line = raw.trim_end();
        if line.trim_start().starts_with("# ==== map") {
            let map = line
                .strip_prefix("# ==== map: ")
                .and_then(|rest| rest.strip_suffix(" ===="))
                .ok_or_else(|| {
                    format!(
                        "ui-theme.toml line {}: `{}` is not a map sentinel; spell it `# ==== map: <map> ====`",
                        i + 1,
                        line.trim()
                    )
                })?;
            if let Some(problem) = map_problem(map) {
                return Err(format!("ui-theme.toml line {}: sentinel map {problem}", i + 1));
            }
            if maps.iter().any(|m| m == map) {
                return Err(format!(
                    "ui-theme.toml line {}: map `{map}` has two sentinels; a map has exactly one",
                    i + 1
                ));
            }
            if lines.get(i + 1).is_none_or(|next| !next.trim().is_empty()) {
                return Err(format!(
                    "ui-theme.toml line {}: the sentinel of `{map}` must be followed by a blank line",
                    i + 1
                ));
            }
            maps.push(map.to_string());
            current = Some(map.to_string());
        } else if line.trim() == "[[map]]" {
            at_header.push(current.clone());
        }
    }
    Ok((maps, at_header))
}

/// An optional string field: absent is `None`, a non-string is refused.
fn optional_string<'a>(t: &'a Table, key: &str, at: &str) -> Result<Option<&'a str>, String> {
    match t.get(key) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(other) => Err(format!("ui-theme.toml: {at}: `{key}` is not a string: {other}")),
    }
}

/// An optional colour part: absent is `none`; a word that is not a role is refused, with the vocabulary.
fn optional_role(t: &Table, key: &str, at: &str) -> Result<ColourRole, String> {
    let Some(word) = optional_string(t, key, at)? else { return Ok(ColourRole::None) };
    ColourRole::from_key(word).ok_or_else(|| {
        let all: Vec<&str> = ColourRole::ALL.iter().map(|r| r.key()).collect();
        format!(
            "ui-theme.toml: {at}: `{key}` {word:?} is not a colour role; the roles are {}",
            all.join(", ")
        )
    })
}

/// The `word`: one line, something to show, no space at either end (which nothing would see).
fn parse_word(t: &Table, at: &str) -> Result<Option<String>, String> {
    let Some(w) = optional_string(t, "word", at)? else { return Ok(None) };
    if w.is_empty() || w != w.trim() || w.contains(['\n', '\r']) {
        return Err(format!(
            "ui-theme.toml: {at}: `word` {w:?} is empty, runs over more than one line, or has a space at either end"
        ));
    }
    Ok(Some(w.to_string()))
}

/// The `icon`: the key of an icon of the kit's registry, which this looks up in `icons::ALL` itself.
fn parse_icon(t: &Table, at: &str) -> Result<Option<String>, String> {
    let Some(key) = optional_string(t, "icon", at)? else { return Ok(None) };
    if icons::ALL.iter().any(|(k, _)| *k == key) {
        return Ok(Some(key.to_string()));
    }
    let keys: Vec<&str> = icons::ALL.iter().map(|(k, _)| *k).collect();
    Err(format!(
        "ui-theme.toml: {at}: `icon` {key:?} is not a key of the icon registry (`registry!` in crates/vike-ui-theme/src/icons.rs: the constant's name, as `CONNECTIONS`); the keys are {}",
        keys.join(", ")
    ))
}

/// The `count`: a whole number that fits a `u8`.
fn parse_count(t: &Table, at: &str) -> Result<Option<u8>, String> {
    match t.get("count") {
        None => Ok(None),
        Some(Value::Integer(i)) if (0..=255).contains(i) => Ok(Some(*i as u8)),
        Some(Value::Integer(i)) => {
            Err(format!("ui-theme.toml: {at}: `count` {i} does not fit u8 (0..=255)"))
        }
        Some(Value::Float(f)) => Err(format!(
            "ui-theme.toml: {at}: `count` {f} is not a whole number: a u8 is written without a decimal point"
        )),
        Some(other) => Err(format!("ui-theme.toml: {at}: `count` {other} is not a whole number")),
    }
}

/// The `flag`: `true` or `false`.
fn parse_flag(t: &Table, at: &str) -> Result<Option<bool>, String> {
    match t.get("flag") {
        None => Ok(None),
        Some(Value::Boolean(b)) => Ok(Some(*b)),
        Some(other) => Err(format!("ui-theme.toml: {at}: `flag` {other} is not `true` or `false`")),
    }
}

/// One `[[map]]` table, checked. `n` is its place in the file (1-based), for the message.
fn row(n: usize, t: &Table, declared: &BTreeSet<String>) -> Result<Row, String> {
    let at = format!("[[map]] #{n}");
    if let Some(key) = t.keys().find(|k| !ROW_KEYS.contains(&k.as_str())) {
        return Err(format!(
            "ui-theme.toml: {at} has an unknown key `{key}` (a row has {})",
            ROW_KEYS.join(", ")
        ));
    }
    let map = string(t, "map", &at)?;
    let key = string(t, "key", &at)?;
    let at = format!("{at} `{map}.{key}`");
    if let Some(problem) = map_problem(map) {
        return Err(format!("ui-theme.toml: {at}: `map` {problem}"));
    }
    if !declared.contains(map) {
        return Err(format!(
            "ui-theme.toml: {at}: map `{map}` has no sentinel; add `{}` (and a blank line) above its rows",
            sentinel(map)
        ));
    }
    if let Some(problem) = key_problem(key) {
        return Err(format!("ui-theme.toml: {at}: `key` {problem}"));
    }
    let doc = string(t, "doc", &at)?.trim();
    if doc.is_empty() || doc.contains(['\n', '\r']) {
        return Err(format!("ui-theme.toml: {at}: `doc` is empty or runs over more than one line"));
    }
    Ok(Row {
        map: map.to_string(),
        key: key.to_string(),
        colour: optional_role(t, "colour", &at)?,
        fill: optional_role(t, "fill", &at)?,
        stroke: optional_role(t, "stroke", &at)?,
        text: optional_role(t, "text", &at)?,
        word: parse_word(t, &at)?,
        icon: parse_icon(t, &at)?,
        count: parse_count(t, &at)?,
        flag: parse_flag(t, &at)?,
        doc: doc.to_string(),
    })
}

/// Parses `ui-theme.toml`'s `[[map]]` table out of its text, or says exactly what is wrong.
///
/// The text, not the parsed table, because the maps are declared by comments.
pub fn parse(text: &str) -> Result<Maps, String> {
    let (declared, at_header) = scan_layout(text)?;
    let table: Table = text.parse().map_err(|e| format!("ui-theme.toml: {e}"))?;
    let tables = match table.get("map") {
        None => Vec::new(),
        Some(Value::Array(a)) => a.iter().collect(),
        Some(_) => {
            return Err("ui-theme.toml: `map` must be an array of tables, `[[map]]`".into());
        }
    };
    if tables.len() != at_header.len() {
        return Err(format!(
            "ui-theme.toml: {} rows but {} `[[map]]` headers: write each row as its own `[[map]]` table",
            tables.len(),
            at_header.len()
        ));
    }
    let known: BTreeSet<String> = declared.iter().cloned().collect();
    let mut rows: Vec<Row> = Vec::with_capacity(tables.len());
    for (i, t) in tables.iter().enumerate() {
        let t = t
            .as_table()
            .ok_or_else(|| format!("ui-theme.toml: [[map]] #{} is not a table", i + 1))?;
        let r = row(i + 1, t, &known)?;
        if at_header[i].as_deref() != Some(r.map.as_str()) {
            let under = at_header[i]
                .as_deref()
                .map_or("no map sentinel at all".to_string(), |m| format!("`{}`", sentinel(m)));
            return Err(format!(
                "ui-theme.toml: [[map]] #{} `{}.{}` sits under {under}; move it under `{}`",
                i + 1,
                r.map,
                r.key,
                sentinel(&r.map)
            ));
        }
        if let Some(first) = rows.iter().position(|p| p.map == r.map && p.key == r.key) {
            return Err(format!(
                "ui-theme.toml: duplicate map row `{}.{}` (rows #{} and #{})",
                r.map,
                r.key,
                first + 1,
                i + 1
            ));
        }
        rows.push(r);
    }
    // Alphabetical maps, TOML order within each: the output depends on the SET of rows, not on where anyone
    // appended them.
    rows.sort_by(|a, b| a.map.cmp(&b.map));
    Ok(Maps { maps: known.into_iter().collect(), rows })
}

// ---- the Rust ----------------------------------------------------------------------------------

/// An optional string as the Rust writes it: `None` or `Some("…")`.
fn some_str(v: &Option<String>) -> String {
    v.as_ref().map_or("None".to_string(), |s| format!("Some({s:?})"))
}

/// `src/map_tokens.rs`: one `pub mod` per declared map, alphabetical, each holding its constants and `ALL`,
/// then `MAPS`.
pub fn rust(ms: &Maps) -> String {
    let mut s = String::from(RUST_HEADER);
    for m in &ms.maps {
        let rows: Vec<&Row> = ms.in_map(m).collect();
        let _ = writeln!(
            s,
            "/// {}: the `[[map]]` rows of `ui-theme.toml` whose `map` is `{m}`.",
            label(m)
        );
        let _ = writeln!(s, "pub mod {m} {{");
        for r in &rows {
            let _ = writeln!(s, "    /// {}", doc_comment(&r.doc));
            let _ = writeln!(
                s,
                "    pub const {}: super::MapRow = super::MapRow {{ key: {:?}, colour: super::ColourRole::{:?}, fill: super::ColourRole::{:?}, stroke: super::ColourRole::{:?}, text: super::ColourRole::{:?}, word: {}, icon: {}, count: {}, flag: {}, doc: {:?} }};",
                r.key,
                r.key,
                r.colour,
                r.fill,
                r.stroke,
                r.text,
                some_str(&r.word),
                some_str(&r.icon),
                r.count.map_or("None".to_string(), |c| format!("Some({c})")),
                r.flag.map_or("None".to_string(), |f| format!("Some({f})")),
                r.doc
            );
        }
        if !rows.is_empty() {
            s.push('\n');
        }
        let all: Vec<String> = rows.iter().map(|r| format!("&{}", r.key)).collect();
        let _ = writeln!(
            s,
            "    /// Every row of this map, in the order the TOML lists them.\n    pub const ALL: &[&super::MapRow] = &[{}];\n}}\n",
            all.join(", ")
        );
    }
    s.push_str("/// Every map, alphabetical, with its rows: what the brand book and the tests iterate.\npub const MAPS: &[Map] = &[\n");
    for m in &ms.maps {
        let _ = writeln!(s, "    Map {{ name: {m:?}, rows: {m}::ALL }},");
    }
    s.push_str("];\n");
    s
}

// ---- the page ----------------------------------------------------------------------------------

/// A role as a table cell: a swatch of the colour it is in Graphite beside its word, or a dash for `none`.
fn role_cell(t: &Tokens, role: ColourRole) -> String {
    if role == ColourRole::None {
        return "—".to_string();
    }
    format!(
        "<i class=\"msw\" style=\"background:{}\"></i><code>{}</code>",
        role_css(t, PAGE_THEME, role),
        role.key()
    )
}

/// The glyph of the icon registered under `key`, as the character reference the page prints, drawn in the
/// Phosphor face the page loads.
pub fn glyph(key: &str) -> Option<String> {
    let (_, icon) = icons::ALL.iter().find(|(k, _)| *k == key)?;
    let text = icon.rich().text().to_string();
    let c = text.chars().next()?;
    Some(format!("&#x{:X};", c as u32))
}

/// The page's cell for an icon: its real glyph and its key, or a dash.
fn icon_cell(icon: Option<&str>) -> String {
    match icon {
        None => "—".to_string(),
        Some(key) => format!(
            "<span class=\"mico\">{}</span><code>{key}</code>",
            glyph(key).unwrap_or_default()
        ),
    }
}

/// The "Maps" section's body: per declared map a frame (an HTML comment) and, once the map has rows, its heading
/// and a table of them, each role a swatch resolved in Graphite.
pub fn book(t: &Tokens, ms: &Maps) -> String {
    let mut s = String::new();
    for m in &ms.maps {
        let _ = writeln!(s, "<!-- maps: {m} -->");
        let rows: Vec<&Row> = ms.in_map(m).collect();
        if rows.is_empty() {
            continue;
        }
        let _ = writeln!(s, "<h3 id=\"maps-{m}\">{}</h3>", esc(&label(m)));
        s.push_str(
            "<table class=\"maps\"><thead><tr><th>Key</th><th>Colour</th><th>Fill</th><th>Stroke</th><th>Text</th><th>Word</th><th>Icon</th><th>Count</th><th>Flag</th><th>What it is</th></tr></thead><tbody>\n",
        );
        for r in rows {
            let _ = writeln!(
                s,
                "<tr id=\"map-{m}-{}\"><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                r.key,
                r.key,
                role_cell(t, r.colour),
                role_cell(t, r.fill),
                role_cell(t, r.stroke),
                role_cell(t, r.text),
                r.word.as_deref().map_or("—".to_string(), esc),
                icon_cell(r.icon.as_deref()),
                r.count.map_or("—".to_string(), |c| c.to_string()),
                r.flag.map_or("—".to_string(), |f| f.to_string()),
                md_code(&r.doc)
            );
        }
        s.push_str("</tbody></table>\n");
    }
    s
}
