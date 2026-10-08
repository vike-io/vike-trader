//! The general `[[value]]` table of `ui-theme.toml`: every design value that is not a step of one of the
//! three scales (`[[space]]`, `[[stroke]]`, `[[alpha]]`) — a window's own measures, strengths and fixed
//! colours. One row each: `group` (the window or area), `name` (the Rust constant), `kind` (its Rust
//! type), `unit` (what the number MEANS, so the page can draw it), `value` and `doc`.
//!
//! What it writes, from the parsed table:
//!
//! - `src/value_tokens.rs` — `pub mod <group> { pub const NAME: <type> … ; pub const ENTRIES … }` per group,
//!   and `GROUPS`, the registry the page and the tests iterate. Each registry entry is built FROM the
//!   constant (`Data::F32(W_SYMBOL)`), so comparing it with the TOML compares the value the app compiles.
//! - `--value-<group>-<name>` variables for `assets/brand/ui-theme.css`.
//! - the "Window values" section of `assets/brand/brand-book.html`.
//!
//! # Built so that parallel edits merge
//!
//! Many people add rows to this table at once, each to their own window. Two things make that
//! conflict-free, and both are gated by `tests/value_table.rs`:
//!
//! 1. In the TOML, every group has ONE sentinel line (`# ==== value: trade ====`, then a blank line), and its
//!    rows sit under it. Two groups' edits are therefore different places in the file, never adjacent.
//!    A group is DECLARED by its sentinel: a row naming a group with none is refused, and so is a row
//!    that sits under another group's sentinel.
//! 2. Every output holds the groups in alphabetical order whatever order the TOML lists them, and gives
//!    EVERY declared group a frame of its own (a `pub mod`, a CSS comment, an HTML comment) even while it
//!    has no rows. A group's rows therefore appear between lines that never change, so the first row of
//!    one group and the first row of the next are separated by an unchanged line — which is what a
//!    three-way merge needs to take both.
//!
//! Every refusal is a `Result::Err` with a message naming the row, so a bad table stops the generator
//! (see [`super::Tokens::load`]) and a unit test can plant a bad table and read the message.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use toml::{Table, Value};

use super::{RUST_HEADER, esc, kebab, md_code};

/// Rust keywords (strict, reserved and edition-2024), none of which may be a group's module name (nor a map's:
/// `maps.rs` reads this list).
pub(super) const KEYWORDS: &[&str] = &[
    "abstract", "as", "async", "await", "become", "box", "break", "const", "continue", "crate",
    "do", "dyn", "else", "enum", "extern", "false", "final", "fn", "for", "gen", "if", "impl",
    "in", "let", "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub", "ref",
    "return", "self", "static", "struct", "super", "trait", "true", "try", "type", "typeof",
    "unsafe", "unsized", "use", "virtual", "where", "while", "yield",
];

/// The constant a group's module holds besides its rows: a row may not take its name.
const RESERVED_NAME: &str = "ENTRIES";

/// The keys a row may carry.
const ROW_KEYS: [&str; 6] = ["group", "name", "kind", "unit", "value", "doc"];

/// The widest a drawn bar goes before the page fades its end (a longer one is clipped by its cell anyway).
const BAR_FADES_ABOVE: f32 = 360.0;

/// A scaled-down rectangle is drawn at a quarter size when a side is longer than this.
const BOX_FULL_SIZE_UP_TO: f32 = 80.0;

/// What a row's `value` is, in Rust: the whitelist `kind` chooses from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    F32,
    /// For a number the code works with in `f64` (egui_plot's x-units are `f64`: a width in bar widths).
    F64,
    U8,
    I8,
    U16,
    Vec2,
    Colour,
}

impl Kind {
    pub const ALL: [Kind; 7] =
        [Kind::F32, Kind::F64, Kind::U8, Kind::I8, Kind::U16, Kind::Vec2, Kind::Colour];

    /// The word the TOML writes.
    pub fn key(self) -> &'static str {
        match self {
            Kind::F32 => "f32",
            Kind::F64 => "f64",
            Kind::U8 => "u8",
            Kind::I8 => "i8",
            Kind::U16 => "u16",
            Kind::Vec2 => "vec2",
            Kind::Colour => "colour",
        }
    }

    /// The Rust type of the constant.
    pub fn rust_type(self) -> &'static str {
        match self {
            Kind::F32 => "f32",
            Kind::F64 => "f64",
            Kind::U8 => "u8",
            Kind::I8 => "i8",
            Kind::U16 => "u16",
            Kind::Vec2 => "egui::Vec2",
            Kind::Colour => "egui::Color32",
        }
    }

    /// The `vike_ui_theme::value::Kind` variant.
    fn variant(self) -> &'static str {
        match self {
            Kind::F32 => "F32",
            Kind::F64 => "F64",
            Kind::U8 => "U8",
            Kind::I8 => "I8",
            Kind::U16 => "U16",
            Kind::Vec2 => "Vec2",
            Kind::Colour => "Colour",
        }
    }

    fn parse(key: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.key() == key)
    }

    /// The units this kind may carry, for the message that names them.
    fn units(self) -> &'static str {
        match self {
            Kind::F32 | Kind::F64 => "px, ratio or alpha",
            Kind::U8 => "px, alpha or count",
            Kind::I8 | Kind::Vec2 => "px",
            Kind::U16 => "px or count",
            Kind::Colour => "none",
        }
    }
}

/// What a number MEANS, which is what lets the page draw it: a length is a bar, a strength a wash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    /// A length in points (1 pt = 1 CSS px on the page).
    Px,
    /// A unitless factor, drawn from 0 to 1 as a meter.
    Ratio,
    /// An opacity on the 0..=255 byte scale the code stores, drawn as a wash of that strength.
    Alpha,
    /// A whole number that is neither: a count, a limit.
    Count,
}

impl Unit {
    pub const ALL: [Unit; 4] = [Unit::Px, Unit::Ratio, Unit::Alpha, Unit::Count];

    pub fn key(self) -> &'static str {
        match self {
            Unit::Px => "px",
            Unit::Ratio => "ratio",
            Unit::Alpha => "alpha",
            Unit::Count => "count",
        }
    }

    /// The `vike_ui_theme::value::Unit` variant.
    fn variant(self) -> &'static str {
        match self {
            Unit::Px => "Px",
            Unit::Ratio => "Ratio",
            Unit::Alpha => "Alpha",
            Unit::Count => "Count",
        }
    }

    fn parse(key: &str) -> Option<Unit> {
        Unit::ALL.into_iter().find(|u| u.key() == key)
    }
}

/// Whether a `kind` may carry `unit`. A colour carries none (handled by the caller).
fn unit_fits(kind: Kind, unit: Unit) -> bool {
    matches!(
        (kind, unit),
        (Kind::F32 | Kind::F64, Unit::Px | Unit::Ratio | Unit::Alpha)
            | (Kind::U8, Unit::Px | Unit::Alpha | Unit::Count)
            | (Kind::I8, Unit::Px)
            | (Kind::U16, Unit::Px | Unit::Count)
            | (Kind::Vec2, Unit::Px)
    )
}

/// A row's value, parsed and checked to fit its kind.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Data {
    F32(f32),
    F64(f64),
    U8(u8),
    I8(i8),
    U16(u16),
    Vec2([f32; 2]),
    /// Red, green, blue, alpha (unmultiplied). `#rrggbb` is alpha 255.
    Colour([u8; 4]),
}

impl Data {
    pub fn kind(&self) -> Kind {
        match self {
            Data::F32(_) => Kind::F32,
            Data::F64(_) => Kind::F64,
            Data::U8(_) => Kind::U8,
            Data::I8(_) => Kind::I8,
            Data::U16(_) => Kind::U16,
            Data::Vec2(_) => Kind::Vec2,
            Data::Colour(_) => Kind::Colour,
        }
    }
}

/// One `[[value]]` row.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub group: String,
    pub name: String,
    /// `None` for a colour, `Some` for everything else.
    pub unit: Option<Unit>,
    pub data: Data,
    pub doc: String,
}

impl Row {
    pub fn kind(&self) -> Kind {
        self.data.kind()
    }
}

/// The parsed table: the declared groups, alphabetical, and the rows grouped under them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Values {
    /// Every group a sentinel declares, in alphabetical order (a group with no row is still here).
    pub groups: Vec<String>,
    /// Every row, ordered by group (alphabetical) and, within a group, as the TOML lists them.
    pub rows: Vec<Row>,
}

impl Values {
    pub fn in_group<'a>(&'a self, group: &'a str) -> impl Iterator<Item = &'a Row> {
        self.rows.iter().filter(move |r| r.group == group)
    }
}

// ---- the table ---------------------------------------------------------------------------------

/// The sentinel line that declares `group`, and above which no row of it may sit.
pub fn sentinel(group: &str) -> String {
    format!("# ==== value: {group} ====")
}

/// Why `group` cannot be a module name, if it cannot.
fn group_problem(group: &str) -> Option<String> {
    let mut chars = group.chars();
    let snake = chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && group.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !snake {
        return Some(format!(
            "`{group}` is not snake_case (a-z, 0-9 and `_`, starting with a letter)"
        ));
    }
    if KEYWORDS.contains(&group) {
        return Some(format!("`{group}` is a Rust keyword, so it cannot name a module"));
    }
    if group == "egui" {
        return Some("`egui` would shadow the egui crate inside the generated module".to_string());
    }
    None
}

/// Why `name` cannot be a constant's name, if it cannot.
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
            "`{RESERVED_NAME}` is the constant each group's module keeps its rows in"
        ));
    }
    None
}

/// The declared groups (file order) and, for each `[[value]]` header, the group whose sentinel is above it.
type Layout = (Vec<String>, Vec<Option<String>>);

/// Reads the sentinels out of the raw text (a comment is invisible to the TOML parser).
fn scan_layout(text: &str) -> Result<Layout, String> {
    let lines: Vec<&str> = text.lines().collect();
    let (mut groups, mut at_header): (Vec<String>, Vec<Option<String>>) = (Vec::new(), Vec::new());
    let mut current: Option<String> = None;
    for (i, raw) in lines.iter().enumerate() {
        let line = raw.trim_end();
        if line.trim_start().starts_with("# ==== value") {
            let group = line
                .strip_prefix("# ==== value: ")
                .and_then(|rest| rest.strip_suffix(" ===="))
                .ok_or_else(|| {
                    format!(
                        "ui-theme.toml line {}: `{}` is not a group sentinel; spell it `# ==== value: <group> ====`",
                        i + 1,
                        line.trim()
                    )
                })?;
            if let Some(problem) = group_problem(group) {
                return Err(format!("ui-theme.toml line {}: sentinel group {problem}", i + 1));
            }
            if groups.iter().any(|g| g == group) {
                return Err(format!(
                    "ui-theme.toml line {}: group `{group}` has two sentinels; a group has exactly one",
                    i + 1
                ));
            }
            if lines.get(i + 1).is_none_or(|next| !next.trim().is_empty()) {
                return Err(format!(
                    "ui-theme.toml line {}: the sentinel of `{group}` must be followed by a blank line",
                    i + 1
                ));
            }
            groups.push(group.to_string());
            current = Some(group.to_string());
        } else if line.trim() == "[[value]]" {
            at_header.push(current.clone());
        }
    }
    Ok((groups, at_header))
}

pub(super) fn string<'a>(t: &'a Table, key: &str, ctx: &str) -> Result<&'a str, String> {
    t.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("ui-theme.toml: {ctx}: `{key}` is missing or not a string"))
}

/// A TOML number (integer or float) as `f32`, which must be finite.
fn finite(v: &Value) -> Option<f32> {
    let n = match v {
        Value::Float(f) => *f,
        Value::Integer(i) => *i as f64,
        _ => return None,
    };
    let narrow = n as f32;
    narrow.is_finite().then_some(narrow)
}

/// A TOML number (integer or float) as `f64`, which must be finite. Not narrowed: an `f64` row keeps
/// every digit it is written with.
fn finite64(v: &Value) -> Option<f64> {
    let n = match v {
        Value::Float(f) => *f,
        Value::Integer(i) => *i as f64,
        _ => return None,
    };
    n.is_finite().then_some(n)
}

/// An integer within `lo..=hi` of the Rust type `ty`.
fn whole(v: &Value, lo: i64, hi: i64, ty: &str) -> Result<i64, String> {
    match v {
        Value::Integer(i) if (lo..=hi).contains(i) => Ok(*i),
        Value::Integer(i) => Err(format!("`value` {i} does not fit {ty} ({lo}..={hi})")),
        Value::Float(f) => Err(format!(
            "`value` {f} is not a whole number: a {ty} is written without a decimal point"
        )),
        other => Err(format!("`value` {other} is not a whole number")),
    }
}

/// `#rrggbb` or `#rrggbbaa` to its four bytes.
fn colour(v: &Value) -> Result<[u8; 4], String> {
    let hex = v.as_str().ok_or_else(|| format!("`value` {v} is not a string `#rrggbb`"))?;
    let digits = hex
        .strip_prefix('#')
        .filter(|d| (d.len() == 6 || d.len() == 8) && d.chars().all(|c| c.is_ascii_hexdigit()));
    let Some(d) = digits else {
        return Err(format!("`value` {hex:?} is not `#rrggbb` or `#rrggbbaa`"));
    };
    let byte = |i: usize| u8::from_str_radix(&d[i..i + 2], 16).expect("hex digits checked above");
    Ok([byte(0), byte(2), byte(4), if d.len() == 8 { byte(6) } else { 255 }])
}

fn data(kind: Kind, v: &Value) -> Result<Data, String> {
    let not_finite =
        |v: &Value| format!("`value` {v} is not a finite number that fits {}", kind.key());
    match kind {
        Kind::F32 => finite(v).map(Data::F32).ok_or_else(|| not_finite(v)),
        Kind::F64 => finite64(v).map(Data::F64).ok_or_else(|| not_finite(v)),
        Kind::U8 => whole(v, 0, 255, "u8").map(|i| Data::U8(i as u8)),
        Kind::I8 => whole(v, -128, 127, "i8").map(|i| Data::I8(i as i8)),
        Kind::U16 => whole(v, 0, 65535, "u16").map(|i| Data::U16(i as u16)),
        Kind::Vec2 => match v.as_array().map(Vec::as_slice) {
            Some([w, h]) => match (finite(w), finite(h)) {
                (Some(w), Some(h)) => Ok(Data::Vec2([w, h])),
                _ => Err(format!("`value` {v} must hold two finite numbers")),
            },
            _ => Err(format!("`value` {v} is not a pair `[width, height]`")),
        },
        Kind::Colour => colour(v).map(Data::Colour),
    }
}

/// One `[[value]]` table, checked. `n` is its place in the file (1-based), for the message.
fn row(n: usize, t: &Table, declared: &BTreeSet<String>) -> Result<Row, String> {
    let at = format!("[[value]] #{n}");
    if let Some(key) = t.keys().find(|k| !ROW_KEYS.contains(&k.as_str())) {
        return Err(format!(
            "ui-theme.toml: {at} has an unknown key `{key}` (a row has {})",
            ROW_KEYS.join(", ")
        ));
    }
    let group = string(t, "group", &at)?;
    let name = string(t, "name", &at)?;
    let at = format!("{at} `{group}.{name}`");
    if let Some(problem) = group_problem(group) {
        return Err(format!("ui-theme.toml: {at}: `group` {problem}"));
    }
    if !declared.contains(group) {
        return Err(format!(
            "ui-theme.toml: {at}: group `{group}` has no sentinel; add `{}` (and a blank line) above its rows",
            sentinel(group)
        ));
    }
    if let Some(problem) = name_problem(name) {
        return Err(format!("ui-theme.toml: {at}: `name` {problem}"));
    }
    let kind_key = string(t, "kind", &at)?;
    let kind = Kind::parse(kind_key).ok_or_else(|| {
        let all: Vec<&str> = Kind::ALL.iter().map(|k| k.key()).collect();
        format!("ui-theme.toml: {at}: `kind` {kind_key:?} is not one of {}", all.join(", "))
    })?;
    let unit = match (kind, t.get("unit")) {
        (Kind::Colour, None) => None,
        (Kind::Colour, Some(_)) => {
            return Err(format!("ui-theme.toml: {at}: a colour carries no `unit`"));
        }
        (_, None) => {
            return Err(format!(
                "ui-theme.toml: {at}: `unit` is missing; a {} says what its number means: {}",
                kind.key(),
                kind.units()
            ));
        }
        (_, Some(u)) => {
            let word =
                u.as_str().ok_or_else(|| format!("ui-theme.toml: {at}: `unit` is not a string"))?;
            let unit = Unit::parse(word).ok_or_else(|| {
                let all: Vec<&str> = Unit::ALL.iter().map(|u| u.key()).collect();
                format!("ui-theme.toml: {at}: `unit` {word:?} is not one of {}", all.join(", "))
            })?;
            if !unit_fits(kind, unit) {
                return Err(format!(
                    "ui-theme.toml: {at}: a {} cannot be in `{word}`; its unit is {}",
                    kind.key(),
                    kind.units()
                ));
            }
            Some(unit)
        }
    };
    let value = t.get("value").ok_or_else(|| format!("ui-theme.toml: {at}: `value` is missing"))?;
    let data = data(kind, value).map_err(|e| format!("ui-theme.toml: {at}: {e}"))?;
    let doc = string(t, "doc", &at)?.trim();
    if doc.is_empty() || doc.contains(['\n', '\r']) {
        return Err(format!("ui-theme.toml: {at}: `doc` is empty or runs over more than one line"));
    }
    Ok(Row { group: group.to_string(), name: name.to_string(), unit, data, doc: doc.to_string() })
}

/// Parses `ui-theme.toml`'s `[[value]]` table out of its text, or says exactly what is wrong.
///
/// The text, not the parsed table, because the groups are declared by comments.
pub fn parse(text: &str) -> Result<Values, String> {
    let (declared, at_header) = scan_layout(text)?;
    let table: Table = text.parse().map_err(|e| format!("ui-theme.toml: {e}"))?;
    let tables = match table.get("value") {
        None => Vec::new(),
        Some(Value::Array(a)) => a.iter().collect(),
        Some(_) => {
            return Err("ui-theme.toml: `value` must be an array of tables, `[[value]]`".into());
        }
    };
    if tables.len() != at_header.len() {
        return Err(format!(
            "ui-theme.toml: {} rows but {} `[[value]]` headers: write each row as its own `[[value]]` table",
            tables.len(),
            at_header.len()
        ));
    }
    let known: BTreeSet<String> = declared.iter().cloned().collect();
    let mut rows = Vec::with_capacity(tables.len());
    for (i, t) in tables.iter().enumerate() {
        let t = t
            .as_table()
            .ok_or_else(|| format!("ui-theme.toml: [[value]] #{} is not a table", i + 1))?;
        let r = row(i + 1, t, &known)?;
        if at_header[i].as_deref() != Some(r.group.as_str()) {
            let under = at_header[i]
                .as_deref()
                .map_or("no group sentinel at all".to_string(), |g| format!("`{}`", sentinel(g)));
            return Err(format!(
                "ui-theme.toml: [[value]] #{} `{}.{}` sits under {under}; move it under `{}`",
                i + 1,
                r.group,
                r.name,
                sentinel(&r.group)
            ));
        }
        if let Some(first) = rows.iter().position(|p: &Row| p.group == r.group && p.name == r.name)
        {
            return Err(format!(
                "ui-theme.toml: duplicate value `{}.{}` (rows #{} and #{})",
                r.group,
                r.name,
                first + 1,
                i + 1
            ));
        }
        rows.push(r);
    }
    // Alphabetical groups, TOML order within each: the output depends on the SET of rows, not on where
    // anyone appended them.
    rows.sort_by(|a, b| a.group.cmp(&b.group));
    Ok(Values { groups: known.into_iter().collect(), rows })
}

// ---- the text a value is written as ------------------------------------------------------------

/// A scalar as the code and CSS write it: `150`, `0.45`, `-3`.
fn scalar(d: &Data) -> Option<String> {
    Some(match d {
        Data::F32(v) => format!("{v}"),
        Data::F64(v) => format!("{v}"),
        Data::U8(v) => format!("{v}"),
        Data::I8(v) => format!("{v}"),
        Data::U16(v) => format!("{v}"),
        Data::Vec2(_) | Data::Colour(_) => return None,
    })
}

/// A scalar as a number, for the arithmetic of drawing it (an `f64` is narrowed: a drawing is in `f32`
/// points, and the text beside it is written from the full value).
fn amount(d: &Data) -> Option<f32> {
    Some(match d {
        Data::F32(v) => *v,
        Data::F64(v) => *v as f32,
        Data::U8(v) => f32::from(*v),
        Data::I8(v) => f32::from(*v),
        Data::U16(v) => f32::from(*v),
        Data::Vec2(_) | Data::Colour(_) => return None,
    })
}

/// `#rrggbb`, or `#rrggbbaa` when the colour is not opaque.
fn hex(c: [u8; 4]) -> String {
    let [r, g, b, a] = c;
    if a == 255 {
        format!("#{r:02x}{g:02x}{b:02x}")
    } else {
        format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
    }
}

/// How the value reads in the table: `150 px`, `0.45`, `30 / 255`, `600 × 560 px`, a colour's hex.
pub fn value_text(d: &Data, unit: Option<Unit>) -> String {
    match (d, unit) {
        (Data::Colour(c), _) => format!("<code>{}</code>", hex(*c)),
        (Data::Vec2([w, h]), _) => format!("{w} × {h} px"),
        (d, Some(Unit::Px)) => format!("{} px", scalar(d).unwrap_or_default()),
        (d, Some(Unit::Alpha)) => format!("{} / 255", scalar(d).unwrap_or_default()),
        (d, _) => scalar(d).unwrap_or_default(),
    }
}

/// "Data manager" words of a group, for its heading: `data_manager` → `Data Manager`.
pub fn label(group: &str) -> String {
    group
        .split('_')
        .map(|w| {
            let mut cs = w.chars();
            cs.next().map(|c| c.to_ascii_uppercase().to_string() + cs.as_str()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ---- the Rust ----------------------------------------------------------------------------------

/// The Rust expression of a value, constructible in a `const`.
fn rust_expr(d: &Data) -> String {
    match d {
        Data::F32(v) => format!("{v:?}"),
        Data::F64(v) => format!("{v:?}"),
        Data::U8(v) => format!("{v}"),
        Data::I8(v) => format!("{v}"),
        Data::U16(v) => format!("{v}"),
        Data::Vec2([w, h]) => format!("egui::vec2({w:?}, {h:?})"),
        Data::Colour([r, g, b, 255]) => format!("egui::Color32::from_rgb({r}, {g}, {b})"),
        Data::Colour([r, g, b, a]) => {
            format!("egui::Color32::from_rgba_unmultiplied_const({r}, {g}, {b}, {a})")
        }
    }
}

/// The `Data` variant a registry entry holds, built from the constant itself.
fn rust_data(r: &Row) -> String {
    format!("super::Data::{}({})", r.kind().variant(), r.name)
}

/// A doc comment line: a `[` or `]` is escaped, because rustdoc reads `[Name]` as an intra-doc link.
pub(super) fn doc_comment(doc: &str) -> String {
    doc.replace('[', "\\[").replace(']', "\\]")
}

/// `src/value_tokens.rs`: one `pub mod` per declared group, alphabetical, each holding its constants
/// and `ENTRIES`, then `GROUPS`.
pub fn rust(vs: &Values) -> String {
    let mut s = String::from(RUST_HEADER);
    for g in &vs.groups {
        let rows: Vec<&Row> = vs.in_group(g).collect();
        let _ = writeln!(
            s,
            "/// {}: the `[[value]]` rows of `ui-theme.toml` whose `group` is `{g}`.",
            label(g)
        );
        let _ = writeln!(s, "pub mod {g} {{");
        for r in &rows {
            let _ = writeln!(s, "    /// {}", doc_comment(&r.doc));
            let _ = writeln!(
                s,
                "    pub const {}: {} = {};",
                r.name,
                r.kind().rust_type(),
                rust_expr(&r.data)
            );
        }
        if !rows.is_empty() {
            s.push('\n');
        }
        s.push_str(
            "    /// Every row of this group as data, in the order the TOML lists them.\n    pub const ENTRIES: &[super::Entry] = &[\n",
        );
        for r in &rows {
            let unit = r
                .unit
                .map_or("None".to_string(), |u| format!("Some(super::Unit::{})", u.variant()));
            let _ = writeln!(
                s,
                "        super::Entry {{ group: {g:?}, name: {:?}, unit: {unit}, value: {}, doc: {:?} }},",
                r.name,
                rust_data(r),
                r.doc
            );
        }
        s.push_str("    ];\n}\n\n");
    }
    s.push_str("/// Every group, alphabetical, with its rows: what the brand book and the tests iterate.\npub const GROUPS: &[Group] = &[\n");
    for g in &vs.groups {
        let _ = writeln!(s, "    Group {{ name: {g:?}, entries: {g}::ENTRIES }},");
    }
    s.push_str("];\n");
    s
}

// ---- the CSS -----------------------------------------------------------------------------------

/// `--value-<group>-<name>`, kebab-case.
pub fn css_var(group: &str, name: &str) -> String {
    format!("--value-{}-{}", kebab(group), kebab(&name.to_lowercase()))
}

/// The variables, a frame (a comment) per declared group and the group's variables after it.
pub fn css(vs: &Values) -> String {
    let mut s = String::new();
    for g in &vs.groups {
        let _ = writeln!(s, "  /* values: {g} */");
        for r in vs.in_group(g) {
            let var = css_var(g, &r.name);
            match (&r.data, r.unit) {
                (Data::Colour(c), _) => {
                    let _ = writeln!(s, "  {var}: {};", hex(*c));
                }
                (Data::Vec2([w, h]), _) => {
                    let _ = writeln!(s, "  {var}-w: {w}px;");
                    let _ = writeln!(s, "  {var}-h: {h}px;");
                }
                (d, Some(Unit::Px)) => {
                    let _ = writeln!(s, "  {var}: {}px;", scalar(d).unwrap_or_default());
                }
                (d, _) => {
                    let _ = writeln!(s, "  {var}: {};", scalar(d).unwrap_or_default());
                }
            }
        }
    }
    s
}

// ---- the page ----------------------------------------------------------------------------------

/// What a row looks like, drawn from its value.
fn drawn(r: &Row) -> String {
    match (&r.data, r.unit) {
        (Data::Colour(c), _) => format!("<i class=\"vsw\" style=\"background:{}\"></i>", hex(*c)),
        (Data::Vec2([w, h]), _) => {
            let (scale, note) = if w.max(*h) > BOX_FULL_SIZE_UP_TO {
                (4.0, " <small>quarter size</small>")
            } else {
                (1.0, "")
            };
            format!(
                "<i class=\"vbox\" style=\"width:{}px;height:{}px\"></i>{note}",
                w / scale,
                h / scale
            )
        }
        (d, Some(Unit::Px)) => {
            let v = amount(d).unwrap_or(0.0);
            if v <= 0.0 {
                return "—".to_string();
            }
            let cut = if v > BAR_FADES_ABOVE { " cut" } else { "" };
            format!("<i class=\"vbar{cut}\" style=\"width:{v}px\"></i>")
        }
        (d, Some(Unit::Ratio)) => match amount(d) {
            Some(v) if (0.0..=1.0).contains(&v) => {
                format!("<i class=\"vmeter\"><i style=\"width:{:.1}%\"></i></i>", v * 100.0)
            }
            _ => "—".to_string(),
        },
        (d, Some(Unit::Alpha)) => {
            let pct = (amount(d).unwrap_or(0.0) / 255.0 * 100.0).clamp(0.0, 100.0);
            format!(
                "<i class=\"wash\" style=\"background:color-mix(in srgb, var(--accent) {pct:.1}%, var(--bg))\"></i>"
            )
        }
        (_, _) => "—".to_string(),
    }
}

/// The "Window values" section's body: per declared group a frame (an HTML comment) and, once the
/// group has rows, its heading and a table of them.
pub fn book(vs: &Values) -> String {
    let mut s = String::new();
    for g in &vs.groups {
        let _ = writeln!(s, "<!-- values: {g} -->");
        let rows: Vec<&Row> = vs.in_group(g).collect();
        if rows.is_empty() {
            continue;
        }
        let _ = writeln!(s, "<h3 id=\"values-{g}\">{}</h3>", esc(&label(g)));
        s.push_str(
            "<table class=\"values\"><colgroup><col style=\"width:20%\"><col style=\"width:12%\"><col style=\"width:36%\"><col></colgroup><thead><tr><th>Token</th><th>Value</th><th>Drawn</th><th>What it is</th></tr></thead><tbody>\n",
        );
        for r in rows {
            let _ = writeln!(
                s,
                "<tr id=\"value-{g}-{}\"><td><code>{}</code></td><td>{}</td><td class=\"drawn\">{}</td><td>{}</td></tr>",
                r.name,
                r.name,
                value_text(&r.data, r.unit),
                drawn(r),
                md_code(&r.doc)
            );
        }
        s.push_str("</tbody></table>\n");
    }
    s
}
