//! Reads `crates/vike-ui-theme/ui-theme.toml` — the one source of the design values — and draws
//! everything derived from it: the Rust constants the app compiles, `assets/brand/ui-theme.css`, and
//! `assets/brand/brand-book.html`.
//!
//! This is a test-support module, not a test: `tests/brand_assets.rs` is the gate that calls it,
//! compares each output with the committed file, and rewrites the ones that differ when
//! `VIKE_REGEN_BRAND_ASSETS=1` (the same switch as the brand icons: they are one family of committed
//! artifacts, each re-derived from its source and compared).
//!
//! The generator PANICS, naming the key, on a value that is missing or malformed: a half-filled
//! token file must fail loudly here and never become half a theme in the app.
//!
//! The general `[[value]]` table — every value that one window owns — is read and written by the
//! `values` submodule, which returns its refusals as `Result`s so `tests/value_table.rs` can plant a bad
//! table and read the message; [`Tokens::load`] turns each into the panic above. The `[[mode]]` table —
//! which colour ROLE each part of an account mode's chip wears — is the `modes` submodule's, with the same
//! shape and `tests/mode_table.rs`. The `[[map]]` table — which colour, word or icon each state or kind of a
//! mapping uses — is the `maps` submodule's, built like `values` (a sentinel per map) and gated by
//! `tests/map_table.rs`.

#![allow(dead_code)] // each test binary that mounts this module uses a different part of it

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use toml::{Table, Value};

/// The general `[[map]]` table: which colour, word or icon each state or kind of a mapping uses.
pub mod maps;
/// The `[[mode]]` table: where an account trades, and the colour role each part of its chip wears.
pub mod modes;
/// The general `[[value]]` table: every design value that is not a step of a scale.
pub mod values;

/// The themes, in `ThemeId::ALL` order — the order the Rust constants and the page follow.
pub const THEMES: [&str; 4] = ["graphite", "midnight", "dusk", "carbon"];
/// A theme's colours, in the order `Theme`'s fields are declared.
pub const THEME_FIELDS: [&str; 12] = [
    "bg",
    "grad_top",
    "surface",
    "card",
    "hover",
    "border",
    "text",
    "text_ui",
    "text2",
    "text3",
    "analysis_line",
    "accent",
];
/// The densities, in `Density::ALL` order, and the five measures each sets (`Metrics`' fields).
pub const DENSITIES: [&str; 3] = ["compact", "normal", "comfortable"];
pub const DENSITY_FIELDS: [&str; 5] = ["control_h", "row_h", "pad", "gap", "header_h"];
/// The three text sizes, in `TextSize::ALL` order.
pub const TEXT_SIZES: [&str; 3] = ["small", "standard", "large"];

/// The four market-colour sets, in `MarketId::ALL` order, by the key `preferences.market_colors` stores.
pub const MARKETS: [&str; 4] = ["classic", "tradingview", "exchange", "colorblind"];
/// A set's four ruled colours, in the order `market.rs`'s `base` returns them.
pub const MARKET_FIELDS: [&str; 4] = ["up", "down", "up_text", "down_text"];

/// The scale tables: `(TOML key, Rust module, CSS prefix, CSS unit, heading)`. Each `[[key]]` array of
/// `name`/`value`/`doc` rows becomes `pub mod {module}` in `metrics_tokens.rs`, one `--{prefix}-{name}`
/// variable each, and a table on the page.
pub const SCALES: [(&str, &str, &str, &str, &str); 3] = [
    ("space", "space", "space", "px", "Spacing"),
    ("stroke", "stroke", "stroke", "px", "Strokes"),
    ("alpha", "alpha", "alpha", "", "Strengths"),
];

/// The header every generated Rust file carries.
const RUST_HEADER: &str = "// @generated from crates/vike-ui-theme/ui-theme.toml by \
crates/vike-ui-theme/tests/tokens_gen — do not edit; change the TOML and regenerate (see \
tests/brand_assets.rs).\n\n";

pub fn toml_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("ui-theme.toml")
}

/// The parsed token file.
pub struct Tokens {
    root: Table,
    /// The general `[[value]]` table, checked (`values.rs`).
    pub values: values::Values,
    /// The `[[mode]]` table, checked (`modes.rs`).
    pub modes: Vec<modes::Row>,
    /// The general `[[map]]` table, checked (`maps.rs`).
    pub maps: maps::Maps,
}

impl Tokens {
    pub fn load() -> Tokens {
        let path = toml_path();
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        Tokens {
            root: text.parse::<Table>().unwrap_or_else(|e| panic!("ui-theme.toml: {e}")),
            values: values::parse(&text).unwrap_or_else(|e| panic!("{e}")),
            modes: modes::parse(&text).unwrap_or_else(|e| panic!("{e}")),
            maps: maps::parse(&text).unwrap_or_else(|e| panic!("{e}")),
        }
    }

    /// The table at `path` (`["themes", "graphite"]`).
    fn table(&self, path: &[&str]) -> &Table {
        let mut t = &self.root;
        for (i, key) in path.iter().enumerate() {
            t = t
                .get(*key)
                .and_then(Value::as_table)
                .unwrap_or_else(|| panic!("ui-theme.toml: [{}] is missing", path[..=i].join(".")));
        }
        t
    }

    fn array_of_tables(&self, key: &str) -> Vec<&Table> {
        self.root
            .get(key)
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("ui-theme.toml: [[{key}]] is missing"))
            .iter()
            .map(|v| v.as_table().unwrap_or_else(|| panic!("ui-theme.toml: [[{key}]] entry")))
            .collect()
    }
}

fn string<'a>(t: &'a Table, key: &str, ctx: &str) -> &'a str {
    t.get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("ui-theme.toml: {ctx}.{key} is missing or not a string"))
}

fn number(t: &Table, key: &str, ctx: &str) -> f64 {
    match t.get(key) {
        Some(Value::Float(f)) => *f,
        Some(Value::Integer(i)) => *i as f64,
        _ => panic!("ui-theme.toml: {ctx}.{key} is missing or not a number"),
    }
}

/// A whole number 0..=255 (`depth_up_alpha = 48`); anything else panics naming the key.
fn byte(t: &Table, key: &str, ctx: &str) -> u8 {
    match t.get(key) {
        Some(Value::Integer(i)) if (0..=255).contains(i) => *i as u8,
        _ => panic!("ui-theme.toml: {ctx}.{key} is missing or not a whole number 0..=255"),
    }
}

/// `#rrggbb` → its three bytes.
fn rgb(hex: &str, ctx: &str) -> [u8; 3] {
    let h = hex.strip_prefix('#').filter(|h| h.len() == 6);
    let byte = |i: usize| h.and_then(|h| u8::from_str_radix(&h[i..i + 2], 16).ok());
    match (byte(0), byte(2), byte(4)) {
        (Some(r), Some(g), Some(b)) => [r, g, b],
        _ => panic!("ui-theme.toml: {ctx} = {hex:?} is not `#rrggbb`"),
    }
}

pub fn colour(t: &Table, key: &str, ctx: &str) -> [u8; 3] {
    rgb(string(t, key, ctx), &format!("{ctx}.{key}"))
}

fn from_rgb([r, g, b]: [u8; 3]) -> String {
    format!("Color32::from_rgb({r}, {g}, {b})")
}

/// A float as Rust source (`25.0`, `10.5`).
fn f32_src(v: f64) -> String {
    format!("{:?}", v as f32)
}

/// A number as CSS carries it (`25`, `10.5`).
fn css_num(v: f64) -> String {
    if v.fract() == 0.0 { format!("{}", v as i64) } else { format!("{v}") }
}

fn kebab(s: &str) -> String {
    s.replace('_', "-")
}

fn hex_of([r, g, b]: [u8; 3]) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

// ---- the Rust ---------------------------------------------------------------------------------

/// `src/theme_tokens.rs`: the four `const X: Theme` tables.
pub fn rust_theme(t: &Tokens) -> String {
    let mut s = String::from(RUST_HEADER);
    for id in THEMES {
        let table = t.table(&["themes", id]);
        let _ = writeln!(s, "const {}: Theme = Theme {{", id.to_uppercase());
        for field in THEME_FIELDS {
            let c = colour(table, field, &format!("themes.{id}"));
            let _ = writeln!(s, "    {field}: {},", from_rgb(c));
        }
        s.push_str("};\n\n");
    }
    s.truncate(s.trim_end().len());
    s.push('\n');
    s
}

/// `src/type_scale_tokens.rs`: the two scales, one size per role in `TextRole::ALL` order.
pub fn rust_type_scale(t: &Tokens) -> String {
    let roles: Vec<&str> = t
        .table(&["type"])
        .get("roles")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("ui-theme.toml: type.roles is missing"))
        .iter()
        .map(|r| r.as_str().expect("type.roles entries are strings"))
        .collect();
    let mut s = String::from(RUST_HEADER);
    for size in TEXT_SIZES {
        let table = t.table(&["type", size]);
        let sizes: Vec<String> =
            roles.iter().map(|r| f32_src(number(table, r, &format!("type.{size}")))).collect();
        let _ = writeln!(
            s,
            "const {}: [f32; {}] = [{}];",
            size.to_uppercase(),
            roles.len(),
            sizes.join(", ")
        );
    }
    s
}

/// `src/metrics_tokens.rs`: the corner radius and one `Metrics` constant per density.
pub fn rust_metrics(t: &Tokens) -> String {
    let mut s = String::from(RUST_HEADER);
    let shape = t.table(&["shape"]);
    let _ = writeln!(s, "/// {}", string(shape, "radius_doc", "shape"));
    let _ = writeln!(s, "pub const RADIUS: u8 = {};\n", byte(shape, "radius", "shape"));
    for id in DENSITIES {
        let table = t.table(&["density", id]);
        let fields: Vec<String> = DENSITY_FIELDS
            .iter()
            .map(|f| format!("{f}: {}", f32_src(number(table, f, &format!("density.{id}")))))
            .collect();
        let _ = writeln!(
            s,
            "const {}: Metrics = Metrics {{ {} }};",
            id.to_uppercase(),
            fields.join(", ")
        );
    }
    for (key, module, _, _, heading) in SCALES {
        let _ = writeln!(s, "\n/// {heading}: `ui-theme.toml`'s `[[{key}]]`.\npub mod {module} {{");
        for entry in t.array_of_tables(key) {
            let name = string(entry, "name", key);
            let v = number(entry, "value", &format!("{key}.{name}"));
            let _ = writeln!(s, "    /// {}", string(entry, "doc", &format!("{key}.{name}")));
            let _ = writeln!(s, "    pub const {name}: f32 = {};", f32_src(v));
        }
        s.push_str("}\n");
    }
    s
}

/// `src/status_tokens.rs`: the five status colours, each with its doc comment.
pub fn rust_status(t: &Tokens) -> String {
    let mut s = String::from(RUST_HEADER);
    for entry in t.array_of_tables("status") {
        let name = string(entry, "name", "status");
        let c = colour(entry, "hex", &format!("status.{name}"));
        let doc = string(entry, "doc", &format!("status.{name}"));
        let _ = writeln!(s, "/// {doc} `{}`.", hex_of(c).to_uppercase());
        let _ = writeln!(s, "pub const {name}: Color32 = {};", from_rgb(c));
    }
    s
}

/// `src/chrome_tokens.rs`: the window chrome's base measures, each with its doc comment.
pub fn rust_chrome(t: &Tokens) -> String {
    let mut s = String::from(RUST_HEADER);
    for entry in t.array_of_tables("chrome") {
        let name = string(entry, "name", "chrome");
        let v = number(entry, "value", &format!("chrome.{name}"));
        let _ = writeln!(s, "/// {}", string(entry, "doc", &format!("chrome.{name}")));
        let _ = writeln!(s, "pub const {name}: f32 = {};", f32_src(v));
    }
    s
}

/// The `MarketId` variant a stored key names: `colorblind` is `ColourBlind`.
fn market_variant(id: &str) -> &'static str {
    match id {
        "classic" => "Classic",
        "tradingview" => "TradingView",
        "exchange" => "Exchange",
        "colorblind" => "ColourBlind",
        other => panic!("ui-theme.toml: market.{other} is not a MarketId key"),
    }
}

/// `src/market_tokens.rs`: the depth and volume strengths, and `base` — each set's four ruled colours
/// as `(up, down, up as text, down as text)`, with the set's own note above its arm.
pub fn rust_market(t: &Tokens) -> String {
    let m = t.table(&["market"]);
    let mut s = String::from(RUST_HEADER);
    let _ = writeln!(s, "/// {}", string(m, "depth_up_doc", "market"));
    let _ = writeln!(s, "pub const DEPTH_UP_ALPHA: u8 = {};", byte(m, "depth_up_alpha", "market"));
    let _ = writeln!(s, "/// {}", string(m, "depth_down_doc", "market"));
    let _ =
        writeln!(s, "pub const DEPTH_DOWN_ALPHA: u8 = {};", byte(m, "depth_down_alpha", "market"));
    let _ = writeln!(s, "/// {}", string(m, "volume_doc", "market"));
    let _ = writeln!(
        s,
        "pub const VOLUME_FACTOR: f32 = {};",
        f32_src(number(m, "volume_factor", "market"))
    );
    s.push_str(
        "\n/// `(up, down, up as text, down as text)` of `id`: each set's four ruled colours.\n\
         fn base(id: MarketId) -> (Color32, Color32, Color32, Color32) {\n    match id {\n",
    );
    for id in MARKETS {
        let table = t.table(&["market", id]);
        let c = |f: &str| from_rgb(colour(table, f, &format!("market.{id}")));
        if let Some(doc) = table.get("doc").and_then(Value::as_str) {
            let _ = writeln!(s, "        // {doc}");
        }
        let _ = writeln!(
            s,
            "        MarketId::{} => ({}, {}, {}, {}),",
            market_variant(id),
            c("up"),
            c("down"),
            c("up_text"),
            c("down_text")
        );
    }
    s.push_str("    }\n}\n");
    s
}

/// `src/heat_tokens.rs`: the ramp's two ends and its opacities.
pub fn rust_heat(t: &Tokens) -> String {
    let h = t.table(&["heat"]);
    let mut s = String::from(RUST_HEADER);
    for (key, name, doc) in [("cool", "COOL", "cool_doc"), ("warm", "WARM", "warm_doc")] {
        let [r, g, b] = colour(h, key, "heat");
        let _ = writeln!(s, "/// {}", string(h, doc, "heat"));
        let _ = writeln!(s, "const {name}: (u8, u8, u8) = ({r}, {g}, {b});");
    }
    let _ = writeln!(s, "/// {}", string(h, "alpha_doc", "heat"));
    let _ = writeln!(s, "const ALPHA_MIN: f32 = {};", f32_src(number(h, "alpha_min", "heat")));
    let _ = writeln!(s, "/// See `ALPHA_MIN`.");
    let _ = writeln!(s, "const ALPHA_SPAN: f32 = {};", f32_src(number(h, "alpha_span", "heat")));
    s
}

/// `src/brand_tokens.rs`: the mark's two oranges, each with its doc comment and its hex.
pub fn rust_brand(t: &Tokens) -> String {
    let mut s = String::from(RUST_HEADER);
    for entry in t.array_of_tables("brand") {
        let name = string(entry, "name", "brand");
        let c = colour(entry, "hex", &format!("brand.{name}"));
        let doc = string(entry, "doc", &format!("brand.{name}"));
        let _ = writeln!(s, "/// {doc} `{}`.", hex_of(c).to_uppercase());
        let _ = writeln!(s, "pub const {name}: Color32 = {};", from_rgb(c));
    }
    s
}

/// Every generated Rust file: `(path under crates/vike-ui-theme/src/, contents)`.
pub fn rust_files(t: &Tokens) -> Vec<(&'static str, String)> {
    vec![
        ("theme_tokens.rs", rust_theme(t)),
        ("type_scale_tokens.rs", rust_type_scale(t)),
        ("metrics_tokens.rs", rust_metrics(t)),
        ("status_tokens.rs", rust_status(t)),
        ("chrome_tokens.rs", rust_chrome(t)),
        ("market_tokens.rs", rust_market(t)),
        ("heat_tokens.rs", rust_heat(t)),
        ("brand_tokens.rs", rust_brand(t)),
        ("value_tokens.rs", values::rust(&t.values)),
        ("mode_tokens.rs", modes::rust(&t.modes)),
        ("map_tokens.rs", maps::rust(&t.maps)),
    ]
}

// ---- the CSS ----------------------------------------------------------------------------------

fn css_colours(t: &Tokens, id: &str) -> String {
    let table = t.table(&["themes", id]);
    let mut s = String::new();
    for field in THEME_FIELDS {
        let c = colour(table, field, &format!("themes.{id}"));
        let _ = writeln!(s, "  --{}: {};", kebab(field), hex_of(c));
    }
    s
}

fn css_type(t: &Tokens, size: &str) -> String {
    let roles = t.table(&["type", size]);
    let mut s = String::new();
    for role in t
        .table(&["type"])
        .get("roles")
        .and_then(Value::as_array)
        .expect("type.roles")
        .iter()
        .map(|r| r.as_str().expect("role"))
    {
        let _ =
            writeln!(s, "  --{role}: {}px;", css_num(number(roles, role, &format!("type.{size}"))));
    }
    s
}

fn css_density(t: &Tokens, id: &str) -> String {
    let table = t.table(&["density", id]);
    let mut s = String::new();
    for f in DENSITY_FIELDS {
        let _ = writeln!(
            s,
            "  --{}: {}px;",
            kebab(f),
            css_num(number(table, f, &format!("density.{id}")))
        );
    }
    s
}

fn css_market(t: &Tokens, id: &str) -> String {
    let table = t.table(&["market", id]);
    let mut s = String::new();
    for f in MARKET_FIELDS {
        let c = colour(table, f, &format!("market.{id}"));
        let _ = writeln!(s, "  --{}: {};", kebab(f), hex_of(c));
    }
    s
}

/// `assets/brand/ui-theme.css`: every value as a CSS variable. Graphite, Standard text and Normal
/// density are the defaults; `data-theme`, `data-text-size` and `data-density` on any element switch
/// the variables beneath it.
pub fn css(t: &Tokens) -> String {
    let mut s = String::from(
        "/* @generated from crates/vike-ui-theme/ui-theme.toml by crates/vike-ui-theme/tests/tokens_gen\n   \
         — do not edit; change the TOML and regenerate (see tests/brand_assets.rs).\n   \
         Graphite, Standard text and Normal density are the defaults; data-theme=\"midnight|dusk|carbon\",\n   \
         data-text-size=\"small|large\", data-density=\"compact|comfortable\" and\n   \
         data-market=\"tradingview|exchange|colorblind\" on any element switch what is beneath it. An HTML mock-up that links this file is drawn with the app's real values. */\n\n",
    );
    s.push_str(":root {\n");
    s.push_str(&css_colours(t, "graphite"));
    for entry in t.array_of_tables("status") {
        let name = string(entry, "name", "status");
        let c = colour(entry, "hex", &format!("status.{name}"));
        let _ = writeln!(s, "  --status-{}: {};", name.to_lowercase(), hex_of(c));
    }
    s.push_str(&css_type(t, "standard"));
    s.push_str(&css_density(t, "normal"));
    for entry in t.array_of_tables("chrome") {
        let name = string(entry, "name", "chrome");
        let v = number(entry, "value", &format!("chrome.{name}"));
        let _ = writeln!(s, "  --chrome-{}: {}px;", kebab(&name.to_lowercase()), css_num(v));
    }
    s.push_str(&css_market(t, "classic"));
    let heat = t.table(&["heat"]);
    for (name, key) in [("cool", "cool"), ("warm", "warm")] {
        let _ = writeln!(s, "  --heat-{name}: {};", hex_of(colour(heat, key, "heat")));
    }
    let _ = writeln!(s, "  --heat-alpha-min: {};", css_num(number(heat, "alpha_min", "heat")));
    let _ = writeln!(s, "  --heat-alpha-span: {};", css_num(number(heat, "alpha_span", "heat")));
    for entry in t.array_of_tables("brand") {
        let name = string(entry, "name", "brand");
        let c = colour(entry, "hex", &format!("brand.{name}"));
        let _ = writeln!(s, "  --brand-{}: {};", kebab(&name.to_lowercase()), hex_of(c));
    }
    for (key, _, prefix, unit, _) in SCALES {
        for entry in t.array_of_tables(key) {
            let name = string(entry, "name", key);
            let v = number(entry, "value", &format!("{key}.{name}"));
            let _ =
                writeln!(s, "  --{prefix}-{}: {}{unit};", kebab(&name.to_lowercase()), css_num(v));
        }
    }
    s.push_str(&values::css(&t.values));
    let _ =
        writeln!(s, "  --radius: {}px;", css_num(number(t.table(&["shape"]), "radius", "shape")));
    let words = t.table(&["fonts", "words"]);
    let numbers = t.table(&["fonts", "numbers"]);
    let _ =
        writeln!(s, "  --font-words: \"{}\", sans-serif;", string(words, "family", "fonts.words"));
    let _ = writeln!(
        s,
        "  --font-numbers: \"{}\", monospace;",
        string(numbers, "family", "fonts.numbers")
    );
    s.push_str("}\n");
    for id in &THEMES[1..] {
        let _ = write!(s, "\n[data-theme=\"{id}\"] {{\n{}}}\n", css_colours(t, id));
    }
    for id in ["small", "large"] {
        let _ = write!(s, "\n[data-text-size=\"{id}\"] {{\n{}}}\n", css_type(t, id));
    }
    for id in [DENSITIES[0], DENSITIES[2]] {
        let _ = write!(s, "\n[data-density=\"{id}\"] {{\n{}}}\n", css_density(t, id));
    }
    for id in &MARKETS[1..] {
        let _ = write!(s, "\n[data-market=\"{id}\"] {{\n{}}}\n", css_market(t, id));
    }
    s
}

// ---- the page ---------------------------------------------------------------------------------

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Inline code in a doc string: `` `x` `` → `<code>x</code>`.
fn md_code(s: &str) -> String {
    let mut out = String::new();
    for (i, part) in esc(s).split('`').enumerate() {
        if i % 2 == 1 {
            let _ = write!(out, "<code>{part}</code>");
        } else {
            out.push_str(part);
        }
    }
    out
}

fn book_themes(t: &Tokens) -> String {
    let mut s = String::new();
    for id in THEMES {
        let table = t.table(&["themes", id]);
        let label = string(table, "label", &format!("themes.{id}"));
        let _ = write!(
            s,
            "<div class=\"theme\" data-theme=\"{id}\"><h3>{}</h3><div class=\"swatches\">",
            esc(label)
        );
        for field in THEME_FIELDS {
            let c = hex_of(colour(table, field, &format!("themes.{id}")));
            let _ = write!(
                s,
                "<div class=\"sw\"><i style=\"background:{c}\"></i><b>{}</b><code>{c}</code></div>",
                esc(&kebab(field))
            );
        }
        s.push_str("</div></div>\n");
    }
    s
}

fn book_status(t: &Tokens) -> String {
    let mut s = String::new();
    for entry in t.array_of_tables("status") {
        let name = string(entry, "name", "status");
        let c = hex_of(colour(entry, "hex", &format!("status.{name}")));
        let doc = string(entry, "doc", &format!("status.{name}"));
        let _ = writeln!(
            s,
            "<div class=\"status\"><i style=\"background:{c}\"></i><div><b>{}</b> <code>{c}</code><p>{}</p></div></div>",
            esc(&name.to_lowercase()),
            md_code(doc)
        );
    }
    s
}

fn book_type(t: &Tokens) -> String {
    let uses = t.table(&["type", "use"]);
    let (small, std, large) =
        (t.table(&["type", "small"]), t.table(&["type", "standard"]), t.table(&["type", "large"]));
    let mut s = String::from(
        "<table><thead><tr><th>Role</th><th>Used for</th><th>Small</th><th>Standard</th><th>Large</th><th>Specimen</th></tr></thead><tbody>\n",
    );
    for role in t
        .table(&["type"])
        .get("roles")
        .and_then(Value::as_array)
        .expect("type.roles")
        .iter()
        .map(|r| r.as_str().expect("role"))
    {
        let _ = writeln!(
            s,
            "<tr><td><b>{role}</b></td><td>{}</td><td>{} px</td><td>{} px</td><td>{} px</td><td><span class=\"spec\" style=\"font-size:var(--{role})\">Binance Spot main <span class=\"num\">85,342.61</span></span></td></tr>",
            md_code(string(uses, role, "type.use")),
            css_num(number(small, role, "type.small")),
            css_num(number(std, role, "type.standard")),
            css_num(number(large, role, "type.large")),
        );
    }
    s.push_str("</tbody></table>\n");
    s
}

fn book_fonts(t: &Tokens) -> String {
    let mut s = String::new();
    for key in ["words", "numbers", "icons"] {
        let table = t.table(&["fonts", key]);
        let weights: Vec<String> = table
            .get("weights")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("ui-theme.toml: fonts.{key}.weights"))
            .iter()
            .map(|w| format!("{}", w.as_integer().expect("weights are integers")))
            .collect();
        let _ = writeln!(
            s,
            "<tr><td><b>{}</b></td><td>{}</td><td>{}</td></tr>",
            esc(string(table, "family", &format!("fonts.{key}"))),
            weights.join(", "),
            md_code(string(table, "use", &format!("fonts.{key}"))),
        );
    }
    s
}

/// A rule's `checked_by`: the `path::test fn` names that fail when the code breaks it.
pub fn checked_by<'a>(rule: &'a Table, id: &str) -> Vec<&'a str> {
    rule.get("checked_by")
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!("ui-theme.toml: rule.{id}.checked_by is missing (`[]` says words only)")
        })
        .iter()
        .map(|v| {
            v.as_str().unwrap_or_else(|| {
                panic!("ui-theme.toml: rule.{id}.checked_by entry is not a string")
            })
        })
        .collect()
}

fn book_rules(t: &Tokens) -> String {
    let mut s = String::new();
    for rule in t.array_of_tables("rule") {
        let id = string(rule, "id", "rule");
        let by = checked_by(rule, id);
        let line = if by.is_empty() {
            "<small class=\"by none\">Words only — no test fails if this is broken.</small>"
                .to_string()
        } else {
            let names: Vec<String> =
                by.iter().map(|n| format!("<code>{}</code>", esc(n))).collect();
            format!("<small class=\"by\">Enforced by {}</small>", names.join(", "))
        };
        let _ = writeln!(
            s,
            "<li id=\"{}\">{}{line}</li>",
            esc(id),
            md_code(string(rule, "text", &format!("rule.{id}")))
        );
    }
    s
}

/// The places a weight heavier than regular is ruled (`[[weight_use]]`).
fn book_weights(t: &Tokens) -> String {
    let mut s = String::from(
        "<table><thead><tr><th>Id</th><th>Weight</th><th>Where</th></tr></thead><tbody>
",
    );
    for entry in t.array_of_tables("weight_use") {
        let id = string(entry, "id", "weight_use");
        let _ = writeln!(
            s,
            "<tr><td><code>{}</code></td><td>{}</td><td>{}</td></tr>",
            esc(id),
            css_num(number(entry, "weight", &format!("weight_use.{id}"))),
            md_code(string(entry, "where", &format!("weight_use.{id}"))),
        );
    }
    s.push_str(
        "</tbody></table>
",
    );
    s
}

/// `rgba(r, g, b, a)` of the heat ramp at `t`, and the 0..=255 alpha `heat.rs`'s `ramp` gives it: the
/// same arithmetic, in the same `f32`, so the strip on the page is the ramp the app paints.
fn heat_cell(t: &Tokens, step: f32) -> (String, u8) {
    let h = t.table(&["heat"]);
    let [r, g, b] = colour(h, if step < 0.5 { "cool" } else { "warm" }, "heat");
    let min = number(h, "alpha_min", "heat") as f32;
    let span = number(h, "alpha_span", "heat") as f32;
    let a = ((min + span * step) * 255.0).round() as u8;
    (format!("rgba({r}, {g}, {b}, {:.3})", f32::from(a) / 255.0), a)
}

fn book_market(t: &Tokens) -> String {
    let m = t.table(&["market"]);
    let mut s = String::from(
        "<table><thead><tr><th>Set</th><th>Up</th><th>Down</th><th>Up as text</th><th>Down as text</th><th>A P&amp;L and a candle pair</th></tr></thead><tbody>\n",
    );
    for id in MARKETS {
        let table = t.table(&["market", id]);
        let ctx = format!("market.{id}");
        let hex = |f: &str| hex_of(colour(table, f, &ctx));
        let cell = |f: &str| {
            let c = hex(f);
            format!("<td><i class=\"dot\" style=\"background:{c}\"></i> <code>{c}</code></td>")
        };
        let note = table
            .get("doc")
            .and_then(Value::as_str)
            .map(|d| format!("<br><small>{}</small>", md_code(d)))
            .unwrap_or_default();
        let _ = writeln!(
            s,
            "<tr><td><b>{}</b> <code>{id}</code>{note}</td>{}{}{}{}<td><span class=\"num\" style=\"color:{}\">+1,204.50</span> <span class=\"num\" style=\"color:{}\">-86.20</span> <i class=\"dot\" style=\"background:{}\"></i><i class=\"dot\" style=\"background:{}\"></i></td></tr>",
            esc(string(table, "label", &ctx)),
            cell("up"),
            cell("down"),
            cell("up_text"),
            cell("down_text"),
            hex("up_text"),
            hex("down_text"),
            hex("up"),
            hex("down"),
        );
    }
    s.push_str("</tbody></table>\n");
    let _ = writeln!(
        s,
        "<p class=\"lead\" style=\"margin-top:10px\">Depth bars are the colour at alpha {} (up) and {} (down) of 255; volume bars at {} strength.</p>",
        byte(m, "depth_up_alpha", "market"),
        byte(m, "depth_down_alpha", "market"),
        css_num(number(m, "volume_factor", "market")),
    );
    s
}

fn book_heat(t: &Tokens) -> String {
    let h = t.table(&["heat"]);
    let mut s = String::from("<div class=\"heat\">");
    for i in 0..=10u8 {
        let step = f32::from(i) / 10.0;
        let (css, a) = heat_cell(t, step);
        let _ = write!(
            s,
            "<div><i style=\"background:{css}\"></i><small>{step:.1}<br>{a}</small></div>"
        );
    }
    s.push_str("</div>\n<table><tbody>\n");
    for (key, doc) in [("cool", "cool_doc"), ("warm", "warm_doc")] {
        let c = hex_of(colour(h, key, "heat"));
        let _ = writeln!(
            s,
            "<tr><td><b>{key}</b></td><td><code>{c}</code></td><td>{}</td></tr>",
            md_code(string(h, doc, "heat"))
        );
    }
    let _ = writeln!(
        s,
        "<tr><td><b>opacity</b></td><td><code>{} + {} x t</code></td><td>{}</td></tr>",
        css_num(number(h, "alpha_min", "heat")),
        css_num(number(h, "alpha_span", "heat")),
        md_code(string(h, "alpha_doc", "heat"))
    );
    s.push_str("</tbody></table>\n");
    s
}

fn book_brand(t: &Tokens) -> String {
    let mut s = String::new();
    for entry in t.array_of_tables("brand") {
        let name = string(entry, "name", "brand");
        let c = hex_of(colour(entry, "hex", &format!("brand.{name}")));
        let doc = string(entry, "doc", &format!("brand.{name}"));
        let _ = writeln!(
            s,
            "<div class=\"status\"><i style=\"background:{c}\"></i><div><b>{}</b> <code>{c}</code><p>{}</p></div></div>",
            esc(&kebab(&name.to_lowercase())),
            md_code(doc)
        );
    }
    s.push_str("<div class=\"clear\"></div>\n");
    s
}

fn book_shape(t: &Tokens) -> String {
    let shape = t.table(&["shape"]);
    format!(
        "<div class=\"shape\">{} pt</div><p class=\"lead\" style=\"margin-top:8px\">{}</p>\n",
        css_num(number(shape, "radius", "shape")),
        md_code(string(shape, "radius_doc", "shape"))
    )
}

/// One table per scale, each row drawn: a bar as long as the step, a line as wide as the stroke, a wash as
/// strong as the strength, beside the name, the value and what it is.
fn book_scales(t: &Tokens) -> String {
    let mut s = String::new();
    for (key, _, _, unit, heading) in SCALES {
        let _ = writeln!(
            s,
            "<h3>{heading}</h3>\n<table><thead><tr><th>Token</th><th>Value</th><th>Drawn</th><th>What it is</th></tr></thead><tbody>"
        );
        for entry in t.array_of_tables(key) {
            let name = string(entry, "name", key);
            let v = number(entry, "value", &format!("{key}.{name}"));
            let drawn = match key {
                "space" => format!("<i class=\"bar\" style=\"width:{}px\"></i>", css_num(v)),
                "stroke" => {
                    format!("<i class=\"rule\" style=\"border-top-width:{}px\"></i>", css_num(v))
                }
                _ => format!(
                    "<i class=\"wash\" style=\"background:color-mix(in srgb, var(--accent) {}%, var(--bg))\"></i>",
                    css_num(v * 100.0)
                ),
            };
            let _ = writeln!(
                s,
                "<tr><td><code>{name}</code></td><td>{}{unit}</td><td>{drawn}</td><td>{}</td></tr>",
                css_num(v),
                md_code(string(entry, "doc", &format!("{key}.{name}"))),
            );
        }
        s.push_str("</tbody></table>\n");
    }
    s
}

fn book_density(t: &Tokens) -> String {
    let mut s = String::from("<table><thead><tr><th>Density</th>");
    for f in DENSITY_FIELDS {
        let _ = write!(s, "<th>{}</th>", esc(&kebab(f)));
    }
    s.push_str("<th>A control</th></tr></thead><tbody>\n");
    for id in DENSITIES {
        let table = t.table(&["density", id]);
        let _ = write!(s, "<tr data-density=\"{id}\"><td><b>{id}</b></td>");
        for f in DENSITY_FIELDS {
            let _ = write!(s, "<td>{}</td>", css_num(number(table, f, &format!("density.{id}"))));
        }
        s.push_str("<td><span class=\"btn\">Buy 0.010</span></td></tr>\n");
    }
    s.push_str("</tbody></table>\n");
    s
}

fn book_chrome(t: &Tokens) -> String {
    let mut s = String::from(
        "<table><thead><tr><th>Token</th><th>Points</th><th>What it is</th></tr></thead><tbody>\n",
    );
    for entry in t.array_of_tables("chrome") {
        let name = string(entry, "name", "chrome");
        let _ = writeln!(
            s,
            "<tr><td><code>{name}</code></td><td>{}</td><td>{}</td></tr>",
            css_num(number(entry, "value", &format!("chrome.{name}"))),
            md_code(string(entry, "doc", &format!("chrome.{name}"))),
        );
    }
    s.push_str("</tbody></table>\n");
    s
}

/// `assets/brand/brand-book.html`: the page that shows every value, drawn with `ui-theme.css`.
pub fn book(t: &Tokens) -> String {
    let template = include_str!("book.html.tmpl");
    [
        ("{{THEMES}}", book_themes(t)),
        ("{{STATUS}}", book_status(t)),
        ("{{MODES}}", modes::book(t)),
        ("{{MAPS}}", maps::book(t, &t.maps)),
        ("{{TYPE}}", book_type(t)),
        ("{{FONTS}}", book_fonts(t)),
        ("{{MARKET}}", book_market(t)),
        ("{{HEAT}}", book_heat(t)),
        ("{{BRAND}}", book_brand(t)),
        ("{{SHAPE}}", book_shape(t)),
        ("{{RULES}}", book_rules(t)),
        ("{{WEIGHTS}}", book_weights(t)),
        ("{{SCALES}}", book_scales(t)),
        ("{{VALUES}}", values::book(&t.values)),
        ("{{DENSITY}}", book_density(t)),
        ("{{CHROME}}", book_chrome(t)),
    ]
    .iter()
    .fold(template.to_string(), |page, (key, html)| {
        assert!(page.contains(key), "book.html.tmpl has no {key}");
        page.replace(key, html)
    })
}
