//! **The backtest PROFILE's schema, as DATA** — every TOML key the harness deserialises, derived
//! from the type that declares it.
//!
//! # Why this exists
//!
//! A run profile is the whole run as one TOML file, and it is the surface an operator actually
//! types. Nothing published it: the FLAG surface got this treatment in `vike_cli::surface`, while
//! the profile's own keys were described by hand, in prose, on the far side.
//!
//! MEASURED, not feared. The published page describing this schema says `[walkforward]` "carries
//! one knob, `n_splits >= 1`". `harness::profile::WalkforwardCfg` declares NINE fields,
//! and two of them (`purge`, `embargo`) carry refusals of their own. The same page names the grid
//! table `[sweep]`; the field is `paramscan`, and `sweep` is only an alias. Both sentences were
//! right when they were written. That is the failure class this module ends.
//!
//! # Why it is DERIVED rather than declared
//!
//! `vike_cli::surface` is a hand-maintained table, and that is defensible at its size: the facts
//! it carries — does a flag take a value, which sub-verb accepts it — are CONTROL FLOW, and no
//! parser recovers them from the source. A profile key is the opposite. It is a struct field, and
//! every fact a reader wants is already written down beside it: the type, whether serde requires
//! it, what it defaults to, and what it is for. Eighty-odd of them is too many to copy by hand,
//! and a hand copy is exactly how "one knob" happened.
//!
//! So this module PARSES the sources that own the schema, compiled in with `include_str!`. The
//! parse is TOTAL, and that is the property that makes it safe: **a shape the parser cannot read
//! PANICS**, so a schema change that would silently drop a key fails the gate instead of shipping
//! a short table. The export cannot fall behind the type, because there is no second copy of it.
//!
//! ⚠ **The one hand-declared fact about the shape is that `ROOT_STRUCT` is the root.** Every
//! table path below it is DERIVED by walking fields whose type names another parsed struct — so
//! `FeeCfg` publishes as `engine.fee` because `EngineCfg::fee` is the field that reaches it, not
//! because a row here says so. A parsed struct the walk never reaches is a gate failure.
//!
//! # What it deliberately does NOT carry
//!
//! - **`[risk]`'s keys.** Its type is `vike_exec::ProfileRisk`, in another crate; reaching its
//!   source would mean an `include_str!` across a package boundary, which is a path dependency
//!   `crates/vike-ops/tests/packaging_gate.rs` would rightly object to. It publishes as a key
//!   whose nested struct is FOREIGN, naming the file that owns it — a DECLARED hole, not a silent
//!   one. `FOREIGN_STRUCTS` is the whole list.
//! - **A roster whose accepted set is a hand-written list inside a message.** A roster is exported
//!   only where the set is DATA in the source: a serde enum's variants, or a `match` whose arms
//!   ARE the roster (`engine.sizer.kind`). `engine.queue_model` names its three values inside its
//!   own refusal text, and re-deriving that list here would be a SECOND copy of the same hand
//!   list — the thing this module exists to avoid. The refusal ships verbatim instead, which is
//!   the authoritative sentence anyway.
//! - **Argument.** Like `vike_cli::surface`, this module is DATA. Rationale belongs in the module
//!   docs of the code it describes and in `crates/vike-backtest/CLAUDE.md`; a row here carries
//!   what a renderer needs plus the symbol it was read from.
//!
//! # Two gates here are about PRODUCTION code, not about the export
//!
//! Parsing the source made two hand-written lists inside it checkable, and both are lists the
//! source's own comments warn will rot: `tests::the_sizer_wrapping_set_agrees_with_the_arms_that_read_base`
//! holds `SizerCfg::build`'s `matches!` equal to the arms that actually read `base`, and
//! `tests::the_unknown_sizer_kind_message_names_every_arm` holds its unknown-kind message equal
//! to the arm roster. Those catch a bug in the engine, not a stale docs page.
//!
//! # Key SETS are the contract, never key ORDER
//!
//! `serde_json`'s map flavour flips with `preserve_order`, which this crate's DataFusion lanes
//! enable through feature unification. Everything order-bearing here is an ARRAY carrying an
//! explicit name field.
//!
//! ⚠ **Line endings are normalised before anything is parsed.** `include_str!` embeds the file as
//! it sits on disk at COMPILE time, and a Windows checkout of this repo has CRLF where the Linux
//! CI runner has LF (measured: 2061 CRs across the 2061-line production half of one source).
//! Without the normalisation the rendered asset would differ BY BOX, and the fixture gate would
//! pass on one and fail on the other.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// The file name this module publishes, and the name the far side's asset list must carry.
///
/// ⚠ It reaches that list as a BARE LITERAL, because `vike-ops` — which owns the gate holding the
/// rendered asset set equal to the release workflow's and the mirror's — cannot depend on this
/// crate in the direction that would let it name this constant. The same declared hole
/// `vike_cli::surface`'s `CLI_JSON` already carries.
pub const PROFILE_JSON: &str = "profile.json";

/// The schema version, bumped on any change a consumer could not read.
///
/// ⚠ A DECLARATION, not a gate. The far side's fetch reads ONE asset's version and demotes every
/// asset to its committed snapshot only when a fetch or a parse FAILS, so bumping this protects
/// nobody until the far side reads it. Treat a shape change as breaking regardless of the number.
pub const SCHEMA_VERSION: u32 = 1;

/// The plane this schema belongs to.
pub const PLANE: &str = "backtest";

/// The struct the walk starts from — the ONE hand-declared fact about the shape.
pub const ROOT_STRUCT: &str = "BacktestProfile";

/// Repo-relative path of the source that owns the profile schema.
pub const PROFILE_SRC_PATH: &str = "crates/vike-backtest/src/harness/profile.rs";

/// Repo-relative path of the source that owns `[[data.series]]`.
pub const SERIES_SRC_PATH: &str = "crates/vike-backtest/src/hist_replay.rs";

const PROFILE_SRC: &str = include_str!("harness/profile.rs");
const SERIES_SRC: &str = include_str!("hist_replay.rs");

/// One harness module whose refusals govern a profile.
#[derive(Debug, Clone, Copy)]
pub struct HarnessModule {
    /// Repo-relative path — the evidence file for every refusal read out of it.
    pub path: &'static str,
    /// What this module decides about a profile.
    pub owns: &'static str,
    src: &'static str,
}

/// Every harness module that raises a refusal, and what each one decides.
///
/// ⚠ **This list exists because the export was SHORT and nothing said so.** The first version
/// parsed `harness/profile.rs` alone and published 62 refusals as "what the profile refuses" —
/// while `harness/windows.rs` was refusing four more about the very same `[walkforward]` table,
/// including the one an operator is most likely to hit (`step` shorter than `test`). A published
/// page then told a reader that nothing refused it. The schema's own file is where the KEYS live;
/// the refusals are spread across the modules that read them.
///
/// `tests::every_harness_module_that_refuses_is_parsed` walks the real `src/harness` directory, so
/// a NEW module carrying a refusal fails the gate until it is listed here — the export cannot go
/// short again without saying so.
pub const HARNESS_MODULES: &[HarnessModule] = &[
    HarnessModule {
        path: "crates/vike-backtest/src/harness/profile.rs",
        owns: "the schema itself — every key, and the per-key and cross-table validation",
        src: PROFILE_SRC,
    },
    HarnessModule {
        path: "crates/vike-backtest/src/harness/windows.rs",
        owns: "resolving [walkforward] into a window list: the bar-count relations between train, \
               test, step, purge and embargo",
        src: include_str!("harness/windows.rs"),
    },
    HarnessModule {
        path: "crates/vike-backtest/src/harness/walkforward.rs",
        owns: "running the walk: what the bar lane and a single series can carry",
        src: include_str!("harness/walkforward.rs"),
    },
    HarnessModule {
        path: "crates/vike-backtest/src/harness/sweep.rs",
        owns: "expanding [paramscan] into a grid",
        src: include_str!("harness/sweep.rs"),
    },
    HarnessModule {
        path: "crates/vike-backtest/src/harness/optimize.rs",
        owns: "the search over that grid",
        src: include_str!("harness/optimize.rs"),
    },
    HarnessModule {
        path: "crates/vike-backtest/src/harness/euler.rs",
        owns: "the successive-halving schedule a search may run under",
        src: include_str!("harness/euler.rs"),
    },
    HarnessModule {
        path: "crates/vike-backtest/src/harness/registry.rs",
        owns: "resolving [strategy].name to something this binary can build",
        src: include_str!("harness/registry.rs"),
    },
    HarnessModule {
        path: "crates/vike-backtest/src/harness/mod.rs",
        owns: "the error type itself — it carries a Display arm, not a refusal of its own",
        src: include_str!("harness/mod.rs"),
    },
];

/// A refusal site whose message is not a literal at the site: it forwards a value the parser cannot
/// resolve from text.
///
/// ⚠ Declared with a COUNT per module, and gated both ways, so "the parser found no message here"
/// can never quietly mean "this module refuses nothing". Each row says what the site forwards.
pub const INDIRECT_REFUSAL_SITES: &[(&str, usize, &str)] = &[
    (
        "crates/vike-backtest/src/harness/mod.rs",
        1,
        "the `Display` impl's own match arm — `Validation(e) => write!(…)` renders a message, it \
         does not raise one",
    ),
    (
        "crates/vike-backtest/src/harness/registry.rs",
        1,
        "`Validation(e.to_string())` forwards a strategy constructor's own error text, which lives \
         in the strategy rather than here",
    ),
    (
        "crates/vike-backtest/src/harness/profile.rs",
        1,
        "`let bad = |m: String| Validation(m)` — a closure its callers hand a `format!` to, and \
         those calls ARE parsed through the `bad(format!(` marker",
    ),
];

/// One struct a profile field reaches whose source this crate cannot read.
#[derive(Debug, Clone, Copy)]
pub struct ForeignStruct {
    pub strukt: &'static str,
    pub owner: &'static str,
    pub why: &'static str,
}

/// The DECLARED-HOLE list: a struct the walk reaches, owned elsewhere, with the reason.
///
/// A referenced struct that is neither parsed nor named here fails
/// `tests::every_referenced_struct_is_parsed_or_declared_foreign`.
/// ⚠ **EMPTY, and it took a correction to get here.** It carried one row — `ProfileRisk`, the
/// `[risk]` table — on the stated ground that "an `include_str!` across a package boundary is a
/// path dependency the packaging gate refuses". That was WRONG:
/// `crates/vike-ops/tests/packaging_gate.rs` is about the `cargo binstall` URL template and
/// mentions `include_str!` nowhere. So eleven keys were missing from the published reference —
/// among them the per-order notional cap a live mount refuses to start without — because of a
/// justification nobody checked, mine included.
///
/// The cure is the shape to reuse: the crate that OWNS the type exposes its own source as data
/// (`vike_exec::risk_surface`), and this module reads it through the dependency edge that already
/// exists. A foreign struct belongs here only when its owner cannot be made to do that, and the
/// row has to say why in terms somebody can check.
pub const FOREIGN_STRUCTS: &[ForeignStruct] = &[];

/// A named `#[serde(default = "fn")]` whose one-line body is a PATH rather than a literal,
/// answered by the compiler instead of by the parser.
///
/// ⚠ Five arms, and they exist because the parser reads TEXT: `fn default_ac_gamma() -> f64 {
/// crate::impact::AC_GAMMA }` carries no number to read. Naming the SAME constant the function
/// returns is what keeps the published value from drifting — change the constant and this changes
/// with it. A path-bodied named default NOT answered here fails
/// `tests::every_unreadable_named_default_is_resolved`; a stale arm fails its twin.
///
/// `default_params` is the one arm naming no constant: its body CONSTRUCTS an empty table
/// (`toml::Value::Table(Default::default())`), and an empty TOML table has one spelling.
fn resolved_default(default_fn: &str) -> Option<String> {
    Some(match default_fn {
        "default_window_secs" => vike_model::fair::UPDOWN_WINDOW_SECS.to_string(),
        "default_impact_window" => crate::DEFAULT_IMPACT_WINDOW.to_string(),
        "default_ac_gamma" => crate::impact::AC_GAMMA.to_string(),
        "default_ac_eta" => crate::impact::AC_ETA.to_string(),
        "default_params" => "{}".to_string(),
        _ => return None,
    })
}

// ---------------------------------------------------------------------------------------------
// The parsed shapes
// ---------------------------------------------------------------------------------------------

/// One field, exactly as the source declares it.
#[derive(Debug, Clone)]
struct Field {
    name: String,
    ty: String,
    /// The first paragraph of the `///` block, link brackets unwrapped; empty when undocumented.
    doc: String,
    /// The contents of each `#[serde(...)]` attribute, comma parts unsplit.
    serde_attrs: Vec<String>,
}

/// One struct, fields in declaration order.
#[derive(Debug, Clone)]
struct Struct {
    name: String,
    source: &'static str,
    /// `#[serde(deny_unknown_fields)]` — whether the table refuses an undeclared key.
    closed: bool,
    fields: Vec<Field>,
}

/// One serde enum whose unit variants ARE a value roster.
#[derive(Debug, Clone)]
struct Enum {
    name: String,
    source: &'static str,
    /// Lowercased under `rename_all = "lowercase"`; otherwise verbatim.
    members: Vec<String>,
    /// The `#[default]` variant's wire spelling, if one is marked.
    default: Option<String>,
}

/// One refusal, as the source raises it.
#[derive(Debug, Clone)]
struct Refusal {
    /// The enclosing `impl` type, when there is one.
    on_type: Option<String>,
    /// The enclosing function — the evidence symbol, never a line number.
    on_fn: String,
    source: &'static str,
    /// The message VERBATIM. A `format!` hole stays as its template, like `vike_cli::surface`.
    message: String,
}

/// One `[engine.sizer]` kind, with the knobs its own arm requires.
#[derive(Debug, Clone)]
struct SizerKind {
    kind: String,
    requires: Vec<String>,
    wraps_base: bool,
}

/// One key of the published schema.
#[derive(Debug, Clone)]
struct Key {
    table: String,
    key: String,
    path: String,
    ty: String,
    /// Serde REQUIRES the key: a profile without it fails to load.
    ///
    /// ⚠ **This doc used to claim the opposite about `Option<T>`** — that an `Option` with no
    /// `#[serde(default)]` is still required, "and calling that optional would be wrong". MEASURED
    /// against serde with a two-field probe struct: an absent `Option<f64>` with no attribute at
    /// all deserialises to `None` and the parse SUCCEEDS, while an absent `f64` is an error. So the
    /// rule is `no default AND not an Option`, and the old rule would have published eleven of
    /// `ProfileRisk`'s keys as required when every one of them is optional. Nothing in
    /// `harness/profile.rs` exercised the difference — every `Option` there carries an explicit
    /// `#[serde(default)]` — which is exactly why the wrong rule survived being written down.
    required: bool,
    /// The declared type is `Option<…>`.
    nullable: bool,
    shape: &'static str,
    default_kind: &'static str,
    default_fn: Option<String>,
    default_value: Option<String>,
    aliases: Vec<String>,
    nested_struct: Option<String>,
    nested_table: Option<String>,
    nested_is_foreign: bool,
    /// The field re-enters a struct already on its own path (`engine.sizer.base`).
    recursive: bool,
    doc: String,
    /// The declaring struct — the evidence symbol.
    strukt: String,
    source: &'static str,
}

/// One table of the published schema.
#[derive(Debug, Clone)]
struct Table {
    table: String,
    strukt: String,
    closed: bool,
    array_of_tables: bool,
    /// The field that reaches it carries no serde default (the root is required by definition).
    required: bool,
    source: &'static str,
}

/// A field serde never reads from TOML, kept visible so the accounting is not silent.
#[derive(Debug, Clone)]
struct Skipped {
    strukt: String,
    field: String,
    doc: String,
}

// ---------------------------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------------------------

/// Normalise an embedded source: CRLF to LF, and cut the `#[cfg(test)]` half.
///
/// The test half is cut because it quotes expected refusal substrings, and a message a test
/// asserts is not a message the binary raises.
fn production_half(src: &str) -> String {
    let lf = src.replace("\r\n", "\n");
    match lf.find("\n#[cfg(test)]\n") {
        Some(at) => lf[..=at].to_string(),
        None => lf,
    }
}

/// The first paragraph of a `///` block, as one line, with intra-doc link brackets unwrapped.
fn first_paragraph(doc: &[String]) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for line in doc {
        if line.is_empty() {
            break;
        }
        parts.push(line);
    }
    unwrap_links(&parts.join(" "))
}

/// A bracketed intra-doc link becomes its inner text, and a markdown link drops its URL.
fn unwrap_links(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'['
            && let Some(close) = s[i + 1..].find(']')
        {
            let inner = &s[i + 1..i + 1 + close];
            // Not a link if it spans a nested bracket — leave it alone.
            if !inner.contains('[') {
                out.push_str(inner);
                i = i + 1 + close + 1;
                // Drop a following `(url)`.
                if i < b.len()
                    && b[i] == b'('
                    && let Some(end) = s[i..].find(')')
                {
                    i += end + 1;
                }
                continue;
            }
        }
        let ch_len = utf8_len(b[i]);
        out.push_str(&s[i..i + ch_len]);
        i += ch_len;
    }
    out
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

/// Every `pub struct` in one source, with its fields.
///
/// ⚠ **Total by construction.** Four line shapes are recognised inside a struct body — blank, a
/// `///` doc line, a one-line `#[…]` attribute, and `pub name: Type,` — and anything else PANICS.
/// Measured against both sources when this was written: zero unreadable lines. That is what makes
/// a silently dropped key impossible.
fn parse_structs(src: &str, source: &'static str) -> Vec<Struct> {
    let mut out: Vec<Struct> = Vec::new();
    let mut open: Option<Struct> = None;
    let mut doc: Vec<String> = Vec::new();
    let mut attrs: Vec<String> = Vec::new();

    for line in src.lines() {
        if let Some(mut s) = open.take() {
            if line == "}" {
                out.push(s);
                doc.clear();
                attrs.clear();
                continue;
            }
            let t = line.trim();
            if t.is_empty() {
                doc.clear();
                attrs.clear();
                open = Some(s);
                continue;
            }
            if t == "///" {
                doc.push(String::new());
                open = Some(s);
                continue;
            }
            if let Some(d) = t.strip_prefix("/// ") {
                doc.push(d.to_string());
                open = Some(s);
                continue;
            }
            if t.starts_with("#[") && t.ends_with(']') {
                attrs.push(t.to_string());
                open = Some(s);
                continue;
            }
            if let Some(rest) = t.strip_prefix("pub ") {
                let (name, ty) = rest.split_once(": ").unwrap_or_else(|| {
                    panic!(
                        "profile_surface: `pub struct {}` declares a field this parser cannot \
                         read: {line:?} — teach the parser rather than letting a key vanish",
                        s.name
                    )
                });
                let serde_attrs: Vec<String> = attrs
                    .iter()
                    .filter_map(|a| {
                        a.strip_prefix("#[serde(")
                            .and_then(|r| r.strip_suffix(")]"))
                            .map(str::to_string)
                    })
                    .collect();
                s.fields.push(Field {
                    name: name.to_string(),
                    ty: ty.trim_end_matches(',').to_string(),
                    doc: first_paragraph(&doc),
                    serde_attrs,
                });
                doc.clear();
                attrs.clear();
                open = Some(s);
                continue;
            }
            panic!(
                "profile_surface: unreadable line inside `pub struct {}`: {line:?} — four shapes \
                 are recognised (blank, `/// doc`, a one-line `#[attr]`, `pub name: Type,`)",
                s.name
            );
        }

        // Outside a struct body: collect the doc/attr run that may precede a declaration.
        let t = line.trim();
        if t == "///" {
            doc.push(String::new());
            continue;
        }
        if let Some(d) = t.strip_prefix("/// ") {
            doc.push(d.to_string());
            continue;
        }
        if t.starts_with("#[") && t.ends_with(']') {
            attrs.push(t.to_string());
            continue;
        }
        if let Some(rest) = line.strip_prefix("pub struct ") {
            let name = rest.trim_end_matches(" {").to_string();
            assert!(
                !attrs.iter().any(|a| a.contains("rename_all")),
                "profile_surface: `pub struct {name}` carries `rename_all`, which renames every \
                 key — the export would publish the FIELD names and be wrong on all of them"
            );
            let closed = attrs.iter().any(|a| a.contains("deny_unknown_fields"));
            open = Some(Struct { name, source, closed, fields: Vec::new() });
            doc.clear();
            attrs.clear();
            continue;
        }
        doc.clear();
        attrs.clear();
    }
    out
}

/// Every `Deserialize` `pub enum` in one source, as a value roster.
fn parse_enums(src: &str, source: &'static str) -> Vec<Enum> {
    let mut out = Vec::new();
    let mut attrs: Vec<String> = Vec::new();
    let mut open: Option<(Enum, bool)> = None;
    let mut next_is_default = false;

    for line in src.lines() {
        if let Some((mut e, lower)) = open.take() {
            if line == "}" {
                out.push(e);
                attrs.clear();
                next_is_default = false;
                continue;
            }
            let t = line.trim();
            if t == "#[default]" {
                next_is_default = true;
                open = Some((e, lower));
                continue;
            }
            if t.is_empty() || t.starts_with("//") || t.starts_with("#[") {
                open = Some((e, lower));
                continue;
            }
            let variant = t.trim_end_matches(',');
            assert!(
                !(variant.contains('(') || variant.contains('{')),
                "profile_surface: `pub enum {}` has the non-unit variant {variant:?} — it is not a \
                 flat TOML value roster",
                e.name
            );
            let wire = if lower { variant.to_ascii_lowercase() } else { variant.to_string() };
            if next_is_default {
                e.default = Some(wire.clone());
                next_is_default = false;
            }
            e.members.push(wire);
            open = Some((e, lower));
            continue;
        }

        let t = line.trim();
        if t.starts_with("#[") && t.ends_with(']') {
            attrs.push(t.to_string());
            continue;
        }
        if let Some(rest) = line.strip_prefix("pub enum ") {
            if attrs.iter().any(|a| a.contains("Deserialize")) {
                let lower = attrs.iter().any(|a| a.contains("rename_all = \"lowercase\""));
                open = Some((
                    Enum {
                        name: rest.trim_end_matches(" {").to_string(),
                        source,
                        members: Vec::new(),
                        default: None,
                    },
                    lower,
                ));
            }
            attrs.clear();
            continue;
        }
        if !t.starts_with("///") {
            attrs.clear();
        }
    }
    out
}

/// Read a Rust string literal starting at the opening quote, returning it and the byte index just
/// past the closing quote.
///
/// Handles exactly the escapes these sources use — `\"` and a `\`-newline continuation, which Rust
/// collapses together with the next line's leading whitespace — and PANICS on anything else, so an
/// escape this does not model cannot reach the export mangled.
fn read_literal(src: &str, start: usize) -> (String, usize) {
    let b = src.as_bytes();
    assert_eq!(b[start], b'"', "profile_surface: read_literal did not start at a quote");
    let mut bytes: Vec<u8> = Vec::new();
    let mut i = start + 1;
    while i < b.len() {
        match b[i] {
            b'"' => {
                return (
                    String::from_utf8(bytes).expect("a Rust source literal is valid UTF-8"),
                    i + 1,
                );
            }
            b'\\' => {
                let next = *b.get(i + 1).expect("a literal cannot end inside an escape");
                match next {
                    b'"' => {
                        bytes.push(b'"');
                        i += 2;
                    }
                    b'\\' => {
                        bytes.push(b'\\');
                        i += 2;
                    }
                    b'\n' => {
                        // Rust drops the newline AND the next line's leading whitespace.
                        i += 2;
                        while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
                            i += 1;
                        }
                    }
                    // ⚠ A message may carry a REAL newline or tab — several of the harness's
                    // multi-line refusals lay out an example over two lines. Kept as the character
                    // rather than as the two-character escape: a consumer renders the text, and
                    // `\n` on a page is a visible backslash. (The docs generator collapses a
                    // newline inside a table cell, which is its own documented rule.)
                    b'n' => {
                        bytes.push(b'\n');
                        i += 2;
                    }
                    b't' => {
                        bytes.push(b'\t');
                        i += 2;
                    }
                    b'r' => {
                        bytes.push(b'\r');
                        i += 2;
                    }
                    b'\'' => {
                        bytes.push(b'\'');
                        i += 2;
                    }
                    other => panic!(
                        "profile_surface: unmodelled escape `\\{}` in a refusal message — teach \
                         read_literal rather than publishing a mangled string",
                        other as char
                    ),
                }
            }
            _ => {
                let len = utf8_len(b[i]);
                bytes.extend_from_slice(&b[i..i + len]);
                i += len;
            }
        }
    }
    panic!("profile_surface: unterminated string literal");
}

/// Every refusal the source raises, with the symbol that raises it.
///
/// ⚠ **Total by construction, and that is the whole point.** Two construction markers are
/// scanned, every site is classified into one of four shapes — a literal, a `format!`, a const
/// message, or an identifier FORWARD that carries no literal here — and anything else PANICS. A
/// refusal the parser cannot read would otherwise be a refusal the docs never mention, which is
/// the exact shape of the gap this module closes. The forwards are returned as a count rather than
/// dropped, and `INDIRECT_REFUSAL_SITES` declares how many each module has and what each forwards,
/// so "no message found here" can never quietly mean "this module refuses nothing".
fn parse_refusals(src: &str, source: &'static str) -> (Vec<Refusal>, Vec<&'static str>) {
    // Per line: the enclosing `impl` type and `fn` name at that point.
    let mut scope: Vec<(usize, Option<String>, String)> = Vec::new();
    let mut cur_impl: Option<String> = None;
    let mut cur_fn = String::from("<module>");
    let mut off = 0usize;
    for line in src.split('\n') {
        if let Some(rest) = line.strip_prefix("impl ") {
            let head = rest.split(" {").next().unwrap_or(rest);
            let ty = head.split_whitespace().last().unwrap_or(head);
            cur_impl = Some(ty.trim_end_matches('{').trim().to_string());
        } else if !line.is_empty() && !line.starts_with(' ') && !line.starts_with('}') {
            // A new top-level item ends the previous impl block's scope.
            if line.starts_with("fn ") || line.starts_with("pub fn ") {
                cur_impl = None;
            }
        }
        let t = line.trim_start();
        for head in ["pub fn ", "pub(crate) fn ", "fn "] {
            if let Some(rest) = t.strip_prefix(head) {
                if let Some(name) = rest.split('(').next()
                    && !name.is_empty()
                    && !name.contains(' ')
                {
                    cur_fn = name.split('<').next().unwrap_or(name).to_string();
                }
                break;
            }
        }
        scope.push((off, cur_impl.clone(), cur_fn.clone()));
        off += line.len() + 1;
    }
    let scope_at = |at: usize| -> (Option<String>, String) {
        let mut best = (None, String::from("<module>"));
        for (o, im, f) in &scope {
            if *o <= at {
                best = (im.clone(), f.clone());
            } else {
                break;
            }
        }
        best
    };
    let line_at = |at: usize| -> &str {
        let start = src[..at].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let end = src[at..].find('\n').map(|i| at + i).unwrap_or(src.len());
        &src[start..end]
    };

    let mut out = Vec::new();
    let mut indirect: Vec<&'static str> = Vec::new();
    for marker in ["HarnessError::Validation(", "bad(format!("] {
        for (at, _) in src.match_indices(marker) {
            // A doc comment mentioning the type is not a construction site.
            let l = line_at(at).trim_start();
            if l.starts_with("//") {
                continue;
            }
            let mut i = at + marker.len();
            i = skip_ws(src, i);
            if !marker.starts_with("bad") && src[i..].starts_with("format!(") {
                i = skip_ws(src, i + "format!(".len());
            }

            // A CONST message: `Validation(PARAMS_NOT_A_TABLE.into())`. Resolvable, and worth
            // resolving — it names a profile key. The const may live in ANOTHER parsed module
            // (`optimize.rs` imports this one from `sweep.rs`), so the whole set is searched.
            if let Some(name) = const_message_name(&src[i..]) {
                let message = resolve_const_message(&name).unwrap_or_else(|| {
                    panic!(
                        "profile_surface: a refusal forwards the const `{name}`, whose declaration \
                         no parsed harness module carries — add the module to HARNESS_MODULES"
                    )
                });
                let (on_type, on_fn) = scope_at(at);
                out.push(Refusal { on_type, on_fn, source, message });
                continue;
            }

            // An INDIRECT site: the value is an identifier, so there is no literal here to read —
            // a `Display` arm, or a closure forwarding what its caller built. Counted rather than
            // skipped, and `INDIRECT_REFUSAL_SITES` declares the count per module.
            if !src[i..].starts_with('"') {
                assert!(
                    is_indirect_site(&src[i..]),
                    "profile_surface: a refusal site at byte {at} of {source} is neither a \
                     literal, a `format!`, a const message nor an identifier forward — classify \
                     it rather than dropping it: {:?}",
                    &src[i..(i + 60).min(src.len())]
                );
                indirect.push(source);
                continue;
            }

            let (message, _) = read_literal(src, i);
            let (on_type, on_fn) = scope_at(at);
            out.push(Refusal { on_type, on_fn, source, message });
        }
    }
    (out, indirect)
}

/// The name in `Validation(SOME_CONST.into())`, if that is the shape at the cursor.
fn const_message_name(rest: &str) -> Option<String> {
    let name: String = rest
        .chars()
        .take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_')
        .collect();
    if name.len() < 2 || !rest[name.len()..].starts_with(".into()") {
        return None;
    }
    Some(name)
}

/// Resolve a `const NAME: &str = "…";` out of any parsed harness module.
///
/// Cross-module by design: `optimize.rs` raises `sweep::PARAMS_NOT_A_TABLE`, and a per-file lookup
/// would report the message as unresolvable while the string sits one module over.
fn resolve_const_message(name: &str) -> Option<String> {
    for m in HARNESS_MODULES {
        let src = production_half(m.src);
        for prefix in ["pub(crate) const ", "pub const ", "const "] {
            let needle = format!("{prefix}{name}: &str = ");
            if let Some(at) = src.find(&needle) {
                let quote = at + needle.len();
                if src[quote..].starts_with('"') {
                    return Some(read_literal(&src, quote).0);
                }
            }
        }
    }
    None
}

/// Whether the value at the cursor is a bare identifier forward (`e`, `m`, `msg`) rather than a
/// message this parser can read.
fn is_indirect_site(rest: &str) -> bool {
    let ident: String = rest.chars().take_while(|c| c.is_ascii_lowercase() || *c == '_').collect();
    if ident.is_empty() {
        return false;
    }
    // `e)`, `m,`, and — `registry.rs`'s shape — `e.to_string())`: a binding, optionally with a
    // conversion on it. The `.` arm is deliberately narrow: a method call on a LOCAL still carries
    // no literal here, which is the only thing this classification decides.
    rest[ident.len()..].starts_with([')', ',', ' ', '.'])
}

fn skip_ws(src: &str, mut i: usize) -> usize {
    let b = src.as_bytes();
    while i < b.len() && (b[i] == b' ' || b[i] == b'\n' || b[i] == b'\t' || b[i] == b'\r') {
        i += 1;
    }
    i
}

/// The `[engine.sizer]` kind roster, read off `SizerCfg::build`'s own match.
fn parse_sizer_kinds(src: &str) -> Vec<SizerKind> {
    let head = "let sizer: Box<dyn PositionSizer> = match kind.as_str() {";
    let start = src
        .find(head)
        .unwrap_or_else(|| panic!("profile_surface: `{head}` is gone — the sizer roster moved"));
    let body = &src[start + head.len()..];
    let mut out: Vec<SizerKind> = Vec::new();
    let mut cur: Option<(String, String)> = None;
    for line in body.split('\n') {
        let t = line.trim();
        if t == "};" {
            break;
        }
        let arm = t
            .strip_prefix('"')
            .and_then(|r| r.split_once("\" =>").map(|(name, _)| name.to_string()));
        if let Some(name) = arm {
            if let Some((k, text)) = cur.take() {
                out.push(finish_arm(k, &text));
            }
            cur = Some((name, line.to_string()));
            continue;
        }
        if t.starts_with("other =>") {
            if let Some((k, text)) = cur.take() {
                out.push(finish_arm(k, &text));
            }
            break;
        }
        if let Some((_, text)) = cur.as_mut() {
            text.push('\n');
            text.push_str(line);
        }
    }
    if let Some((k, text)) = cur.take() {
        out.push(finish_arm(k, &text));
    }
    assert!(!out.is_empty(), "profile_surface: the sizer match yielded no arms");
    out
}

fn finish_arm(kind: String, text: &str) -> SizerKind {
    let mut requires: Vec<String> = Vec::new();
    for (at, _) in text.match_indices("need(\"") {
        let rest = &text[at + "need(\"".len()..];
        if let Some(end) = rest.find('"') {
            let knob = rest[..end].to_string();
            if !requires.contains(&knob) {
                requires.push(knob);
            }
        }
    }
    let wraps_base = text.contains("self.base.as_ref()");
    SizerKind { kind, requires, wraps_base }
}

/// The wrapping-kind set `SizerCfg::build` checks AFTER its match — a second hand-written list
/// inside production code, which is why it is gated against the arms rather than published twice.
#[cfg(test)]
fn parse_sizer_wrapping_set(src: &str) -> Vec<String> {
    let head = "!matches!(kind.as_str(), ";
    let at = src.find(head).unwrap_or_else(|| {
        panic!("profile_surface: `{head}` is gone — the base-under-scalar-kind refusal moved")
    });
    let rest = &src[at + head.len()..];
    let end = rest.find(')').expect("the matches! arm list closes");
    rest[..end]
        .split('|')
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

// ---------------------------------------------------------------------------------------------
// The walk: struct fields become table paths
// ---------------------------------------------------------------------------------------------

/// Strip `Option<…>` / `Box<…>` wrappers, reporting whether the field was nullable.
fn unwrap_type(ty: &str) -> (String, bool) {
    let mut t = ty.trim().to_string();
    let mut nullable = false;
    loop {
        if let Some(inner) = t.strip_prefix("Option<").and_then(|r| r.strip_suffix('>')) {
            nullable = true;
            t = inner.trim().to_string();
            continue;
        }
        if let Some(inner) = t.strip_prefix("Box<").and_then(|r| r.strip_suffix('>')) {
            t = inner.trim().to_string();
            continue;
        }
        return (t, nullable);
    }
}

struct Walked {
    tables: Vec<Table>,
    keys: Vec<Key>,
    skipped: Vec<Skipped>,
}

fn walk(structs: &BTreeMap<String, Struct>) -> Walked {
    let foreign: BTreeSet<&str> = FOREIGN_STRUCTS.iter().map(|f| f.strukt).collect();
    let mut tables = Vec::new();
    let mut keys = Vec::new();
    let mut skipped = Vec::new();

    // (table path, struct, the ancestry that reached it, required, array-of-tables)
    let mut queue: VecDeque<(String, String, Vec<String>, bool, bool)> = VecDeque::new();
    queue.push_back((
        String::new(),
        ROOT_STRUCT.to_string(),
        vec![ROOT_STRUCT.to_string()],
        true,
        false,
    ));

    while let Some((path, sname, ancestry, required, aot)) = queue.pop_front() {
        let s = structs.get(&sname).unwrap_or_else(|| {
            panic!("profile_surface: the walk reached `{sname}`, which no source declares")
        });
        tables.push(Table {
            table: path.clone(),
            strukt: s.name.clone(),
            closed: s.closed,
            array_of_tables: aot,
            required,
            source: s.source,
        });

        for f in &s.fields {
            let mut parts: Vec<&str> = Vec::new();
            for a in &f.serde_attrs {
                parts.extend(a.split(',').map(str::trim));
            }
            let mut default_kind = "none";
            let mut default_fn: Option<String> = None;
            let mut aliases: Vec<String> = Vec::new();
            let mut wire_name = f.name.clone();
            let mut skip = false;
            for p in &parts {
                if p.is_empty() {
                    continue;
                }
                if *p == "default" {
                    default_kind = "implicit";
                } else if *p == "skip" {
                    skip = true;
                } else if let Some(v) = p.strip_prefix("default = ") {
                    default_kind = "named";
                    default_fn = Some(v.trim_matches('"').to_string());
                } else if let Some(v) = p.strip_prefix("alias = ") {
                    aliases.push(v.trim_matches('"').to_string());
                } else if let Some(v) = p.strip_prefix("rename = ") {
                    wire_name = v.trim_matches('"').to_string();
                } else {
                    panic!(
                        "profile_surface: `{}::{}` carries the serde part `{p}`, which this parser \
                         does not model — it may change the wire key or its requiredness",
                        s.name, f.name
                    );
                }
            }
            if skip {
                skipped.push(Skipped {
                    strukt: s.name.clone(),
                    field: f.name.clone(),
                    doc: f.doc.clone(),
                });
                continue;
            }

            let (bare, nullable) = unwrap_type(&f.ty);
            let key_path =
                if path.is_empty() { wire_name.clone() } else { format!("{path}.{wire_name}") };

            // What kind of TOML value is it?
            let (shape, nested, child_aot) =
                if let Some(inner) = bare.strip_prefix("Vec<").and_then(|r| r.strip_suffix('>')) {
                    let (elem, _) = unwrap_type(inner);
                    if structs.contains_key(&elem) || foreign.contains(elem.as_str()) {
                        ("array_of_tables", Some(elem), true)
                    } else {
                        ("array", None, false)
                    }
                } else if bare.starts_with("BTreeMap<") || bare.starts_with("HashMap<") {
                    ("map", None, false)
                } else if bare == "toml::Table" || bare == "toml::Value" {
                    ("free_form_table", None, false)
                } else if structs.contains_key(&bare) || foreign.contains(bare.as_str()) {
                    ("table", Some(bare.clone()), false)
                } else {
                    ("scalar", None, false)
                };

            let nested_is_foreign = nested.as_deref().map(|n| foreign.contains(n)).unwrap_or(false);
            let recursive =
                nested.as_deref().map(|n| ancestry.iter().any(|a| a == n)).unwrap_or(false);
            let nested_table = nested.as_ref().map(|_| key_path.clone());

            let default_value = match (default_kind, default_fn.as_deref()) {
                ("named", Some(f)) => named_default_value(structs, f),
                _ => None,
            };

            keys.push(Key {
                table: path.clone(),
                key: wire_name.clone(),
                path: key_path.clone(),
                ty: f.ty.clone(),
                // See `Key::required`: serde defaults an absent `Option` to `None` on its own, so
                // only a non-Option field with no declared default is genuinely required.
                required: default_kind == "none" && !nullable,
                nullable,
                shape,
                default_kind,
                default_fn: default_fn.clone(),
                default_value,
                aliases,
                nested_struct: nested.clone(),
                nested_table: nested_table.clone(),
                nested_is_foreign,
                recursive,
                doc: f.doc.clone(),
                strukt: s.name.clone(),
                source: s.source,
            });

            if let Some(child) = nested
                && !nested_is_foreign
                && !recursive
            {
                let mut anc = ancestry.clone();
                anc.push(child.clone());
                queue.push_back((key_path, child, anc, default_kind == "none", child_aot));
            }
        }
    }
    Walked { tables, keys, skipped }
}

/// The value a named `#[serde(default = "fn")]` produces: the literal from its one-line body, or
/// the constant `resolved_default` names when the body is a path.
fn named_default_value(_structs: &BTreeMap<String, Struct>, default_fn: &str) -> Option<String> {
    let src = profile_src();
    let head = format!("fn {default_fn}()");
    let at = src.find(&head).unwrap_or_else(|| {
        panic!("profile_surface: `{head}` names a default function no source declares")
    });
    let open = src[at..].find('{').map(|i| at + i)?;
    let close = src[open..].find('}').map(|i| open + i)?;
    let body = src[open + 1..close].trim();
    let body = body.trim_end_matches(".to_string()").trim_matches('"');
    if body.is_empty() {
        return None;
    }
    // A literal body publishes its own text; a path body is answered by the compiler.
    let literal = body.chars().all(|c| c.is_ascii_digit() || c == '.' || c == '-' || c == '_')
        || body == "true"
        || body == "false"
        || !body.contains("::");
    if literal && !body.contains('(') {
        Some(body.to_string())
    } else {
        resolved_default(default_fn)
    }
}

fn profile_src() -> String {
    production_half(PROFILE_SRC)
}

fn series_src() -> String {
    production_half(SERIES_SRC)
}

/// Every struct the export knows, from both sources, keyed by name.
fn all_structs() -> BTreeMap<String, Struct> {
    let mut out = BTreeMap::new();
    for s in parse_structs(&profile_src(), PROFILE_SRC_PATH) {
        out.insert(s.name.clone(), s);
    }
    // Only the struct the profile's `[[data.series]]` array maps onto is published from the
    // replay source; the rest of that file is runtime config, not a TOML surface.
    for s in parse_structs(&series_src(), SERIES_SRC_PATH) {
        if s.name == "SeriesRef" {
            out.insert(s.name.clone(), s);
        }
    }
    // `[risk]`, read through the owning crate's own export of its source. Same parser, same rules —
    // and the same panic-rather-than-drop totality, so a refactor in `vike-exec` fails this gate
    // instead of silently shortening the published table. See `FOREIGN_STRUCTS` for why this is
    // here rather than declared as a hole.
    for s in parse_structs(
        &production_half(vike_exec::risk_surface::RISK_PROFILE_SRC),
        vike_exec::risk_surface::RISK_PROFILE_SRC_PATH,
    ) {
        if s.name == vike_exec::risk_surface::RISK_STRUCT {
            out.insert(s.name.clone(), s);
        }
    }
    out
}

fn all_enums() -> Vec<Enum> {
    let mut out = parse_enums(&profile_src(), PROFILE_SRC_PATH);
    for e in parse_enums(&series_src(), SERIES_SRC_PATH) {
        if e.name == "SeriesKind" {
            out.push(e);
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------------------------

/// The published assets, by name.
pub fn rendered_files() -> BTreeMap<&'static str, String> {
    let mut out = BTreeMap::new();
    out.insert(PROFILE_JSON, render_profile_json());
    out
}

/// Render `profile.json`. Ends with a newline, like every other text asset this repo publishes.
fn render_profile_json() -> String {
    let structs = all_structs();
    let w = walk(&structs);
    let enums = all_enums();

    // Which enum backs which key, so a renderer can bind a roster to a field without guessing.
    let mut roster_of: BTreeMap<String, String> = BTreeMap::new();
    for k in &w.keys {
        let (bare, _) = unwrap_type(&k.ty);
        if enums.iter().any(|e| e.name == bare) {
            roster_of.insert(k.path.clone(), bare);
        }
    }

    let mut rosters: Vec<serde_json::Value> = enums
        .iter()
        .map(|e| {
            let keys: Vec<&String> =
                roster_of.iter().filter(|(_, v)| **v == e.name).map(|(k, _)| k).collect();
            serde_json::json!({
                "id": e.name,
                "members": e.members,
                "default": e.default,
                "derived_from": format!("the `{}` enum's variants", e.name),
                "keys": keys,
                "evidence": { "file": e.source, "symbol": e.name },
            })
        })
        .collect();

    let sizer = parse_sizer_kinds(&profile_src());
    rosters.push(serde_json::json!({
        "id": "SizerKindName",
        "members": sizer.iter().map(|s| s.kind.clone()).collect::<Vec<_>>(),
        "default": serde_json::Value::Null,
        "derived_from": "the arms of `build`'s `match kind.as_str()`",
        "keys": ["engine.sizer.kind"],
        "evidence": { "file": PROFILE_SRC_PATH, "symbol": "SizerCfg" },
    }));

    let doc = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "plane": PLANE,
        "root_struct": ROOT_STRUCT,
        "sources": [
            { "path": PROFILE_SRC_PATH, "owns": "the profile schema" },
            { "path": SERIES_SRC_PATH, "owns": "[[data.series]]" },
        ],
        "tables": w.tables.iter().map(|t| serde_json::json!({
            "table": t.table,
            "struct": t.strukt,
            "closed": t.closed,
            "array_of_tables": t.array_of_tables,
            "required": t.required,
            "evidence": { "file": t.source, "symbol": t.strukt },
        })).collect::<Vec<_>>(),
        "keys": w.keys.iter().map(|k| serde_json::json!({
            "table": k.table,
            "key": k.key,
            "path": k.path,
            "type": k.ty,
            "required": k.required,
            "nullable": k.nullable,
            "shape": k.shape,
            "default": {
                "kind": k.default_kind,
                "fn": k.default_fn,
                "value": k.default_value,
            },
            "aliases": k.aliases,
            "nested": {
                "struct": k.nested_struct,
                "table": k.nested_table,
                "foreign": k.nested_is_foreign,
                "recursive": k.recursive,
            },
            "roster_id": roster_of.get(&k.path),
            "short": if k.doc.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(k.doc.clone()) },
            "evidence": { "file": k.source, "symbol": k.strukt },
        })).collect::<Vec<_>>(),
        "skipped_fields": w.skipped.iter().map(|s| serde_json::json!({
            "struct": s.strukt,
            "field": s.field,
            "why": s.doc,
        })).collect::<Vec<_>>(),
        "foreign_structs": FOREIGN_STRUCTS.iter().map(|f| serde_json::json!({
            "struct": f.strukt,
            "owner": f.owner,
            "why": f.why,
        })).collect::<Vec<_>>(),
        "rosters": rosters,
        "sizer_kinds": sizer.iter().map(|s| serde_json::json!({
            "kind": s.kind,
            "requires": s.requires,
            "wraps_base": s.wraps_base,
        })).collect::<Vec<_>>(),
        "refusals": refusals_json(&w.keys, &w.tables),
    });
    let mut s = serde_json::to_string_pretty(&doc).expect("the schema document is plain data");
    s.push('\n');
    s
}

/// Every refusal every parsed harness module raises, with the keys its message names.
fn all_refusals() -> (Vec<Refusal>, Vec<&'static str>) {
    let mut all = Vec::new();
    let mut indirect = Vec::new();
    for m in HARNESS_MODULES {
        let (rows, ind) = parse_refusals(&production_half(m.src), m.path);
        all.extend(rows);
        indirect.extend(ind);
    }
    all.sort_by(|a, b| (a.message.clone(), a.source).cmp(&(b.message.clone(), b.source)));
    all.dedup_by(|a, b| a.message == b.message && a.source == b.source);
    (all, indirect)
}

fn refusals_json(keys: &[Key], tables: &[Table]) -> Vec<serde_json::Value> {
    // Which profile keys/tables a message NAMES. It is what lets a renderer put a refusal beside
    // the key it governs instead of in one undifferentiated list — and it is derived from the
    // message text against the parsed schema, never tagged by hand.
    let named_in = |message: &str| -> Vec<String> {
        let mut out: Vec<String> =
            keys.iter().filter(|k| message.contains(&k.path)).map(|k| k.path.clone()).collect();
        for t in tables {
            if !t.table.is_empty() && message.contains(&format!("[{}]", t.table)) {
                out.push(format!("[{}]", t.table));
            }
        }
        out.sort();
        out.dedup();
        out
    };
    let (all, _) = all_refusals();
    all.iter()
        .map(|r| {
            serde_json::json!({
                "module": r.source,
                "on_type": r.on_type,
                "on_fn": r.on_fn,
                "message": r.message,
                "names": named_in(&r.message),
                "evidence": { "file": r.source, "symbol": r.on_fn },
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walked() -> (BTreeMap<String, Struct>, Walked) {
        let structs = all_structs();
        let w = walk(&structs);
        (structs, w)
    }

    #[test]
    fn the_sources_parse_at_all() {
        let structs = all_structs();
        assert!(
            structs.contains_key(ROOT_STRUCT),
            "the root struct `{ROOT_STRUCT}` did not parse out of {PROFILE_SRC_PATH}"
        );
        for (name, s) in &structs {
            assert!(!s.fields.is_empty(), "`{name}` parsed with no fields at all");
        }
    }

    #[test]
    fn every_parsed_struct_is_reached_by_the_walk() {
        let (structs, w) = walked();
        let reached: BTreeSet<&str> = w.tables.iter().map(|t| t.strukt.as_str()).collect();
        let orphans: Vec<&str> =
            structs.keys().map(String::as_str).filter(|n| !reached.contains(n)).collect();
        assert!(
            orphans.is_empty(),
            "these parsed structs are reached by no profile field, so the export publishes their \
             keys under no table: {orphans:?} — wire them to a field, or stop parsing them"
        );
    }

    #[test]
    fn every_referenced_struct_is_parsed_or_declared_foreign() {
        let (structs, w) = walked();
        let declared: BTreeSet<&str> = FOREIGN_STRUCTS.iter().map(|f| f.strukt).collect();
        for k in &w.keys {
            if let Some(n) = &k.nested_struct {
                assert!(
                    structs.contains_key(n) || declared.contains(n.as_str()),
                    "`{}` reaches the struct `{n}`, which is neither parsed nor declared in \
                     FOREIGN_STRUCTS — an undeclared hole in the published schema",
                    k.path
                );
            }
        }
    }

    #[test]
    fn no_foreign_row_is_stale() {
        let (_, w) = walked();
        for f in FOREIGN_STRUCTS {
            assert!(
                w.keys.iter().any(|k| k.nested_struct.as_deref() == Some(f.strukt)),
                "FOREIGN_STRUCTS names `{}`, which no profile field reaches any more — delete the \
                 row",
                f.strukt
            );
        }
    }

    #[test]
    fn key_paths_are_unique() {
        let (_, w) = walked();
        let mut seen = BTreeSet::new();
        for k in &w.keys {
            assert!(seen.insert(k.path.clone()), "two rows publish the key path `{}`", k.path);
        }
    }

    #[test]
    fn the_root_and_every_table_refuse_an_undeclared_key() {
        let (_, w) = walked();
        let open: Vec<&str> =
            w.tables.iter().filter(|t| !t.closed).map(|t| t.table.as_str()).collect();
        assert!(
            open.is_empty(),
            "these tables do not carry `deny_unknown_fields`, so a typo in them is a SILENT \
             no-op: {open:?} — the published claim that a typo is a hard load error would be false"
        );
    }

    #[test]
    fn every_unreadable_named_default_is_resolved() {
        let (_, w) = walked();
        for k in &w.keys {
            if k.default_kind == "named" {
                let f = k.default_fn.as_deref().expect("a named default names its function");
                assert!(
                    k.default_value.is_some(),
                    "`{}`'s default comes from `{f}`, whose body the parser cannot read as a \
                     literal and which `resolved_default` does not answer — add an arm naming the \
                     same constant the function returns",
                    k.path
                );
            }
        }
    }

    #[test]
    fn no_resolved_default_arm_is_stale() {
        let (_, w) = walked();
        let used: BTreeSet<&str> = w.keys.iter().filter_map(|k| k.default_fn.as_deref()).collect();
        for name in [
            "default_window_secs",
            "default_impact_window",
            "default_ac_gamma",
            "default_ac_eta",
            "default_params",
        ] {
            assert!(
                used.contains(name),
                "`resolved_default` answers `{name}`, which no field's default names any more — \
                 delete the arm"
            );
        }
    }

    #[test]
    fn the_sizer_wrapping_set_agrees_with_the_arms_that_read_base() {
        let src = profile_src();
        let arms = parse_sizer_kinds(&src);
        let from_arms: BTreeSet<String> =
            arms.iter().filter(|a| a.wraps_base).map(|a| a.kind.clone()).collect();
        let declared: BTreeSet<String> = parse_sizer_wrapping_set(&src).into_iter().collect();
        assert_eq!(
            from_arms, declared,
            "`SizerCfg::build`'s post-match `matches!` and the arms that actually read `base` \
             disagree. A kind in the arms but not the list has its own base REFUSED; a kind in the \
             list but not the arms accepts a base it never reads. This is a bug in the engine, not \
             in the export — the source's own comment warns about exactly this."
        );
    }

    #[test]
    fn the_unknown_sizer_kind_message_names_every_arm() {
        let src = profile_src();
        let arms = parse_sizer_kinds(&src);
        let msg = parse_refusals(&src, PROFILE_SRC_PATH)
            .0
            .into_iter()
            .find(|r| r.message.starts_with("unknown engine.sizer.kind"))
            .expect("the unknown-kind refusal is still raised")
            .message;
        for a in &arms {
            assert!(
                msg.contains(&a.kind),
                "the unknown-sizer-kind message does not name the arm `{}`, so an operator who \
                 typos is shown a roster the parser does not have: {msg:?}",
                a.kind
            );
        }
    }

    #[test]
    fn every_sizer_kind_requires_something_or_is_declared_bare() {
        let arms = parse_sizer_kinds(&profile_src());
        for a in &arms {
            if a.requires.is_empty() && !a.wraps_base {
                assert_eq!(
                    a.kind, "pass_through",
                    "`{}` requires no knob and wraps no base — only the pass-through sizer \
                     legitimately takes nothing",
                    a.kind
                );
            }
        }
    }

    /// ⚠ **Every harness module that raises a refusal must be PARSED.**
    ///
    /// This walks the real `src/harness` directory rather than a list, because the defect it
    /// exists for was an ABSENCE: the export parsed `profile.rs` alone and published its 62
    /// refusals as "what the profile refuses", while four more about the same `[walkforward]`
    /// table sat in `windows.rs` — and the docs page then told a reader that nothing refused a
    /// `step` shorter than `test`. Nothing could have noticed, because a short list looks exactly
    /// like a complete one.
    #[test]
    fn every_harness_module_that_refuses_is_parsed() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/harness");
        let parsed: BTreeSet<&str> =
            HARNESS_MODULES.iter().map(|m| m.path.rsplit('/').next().unwrap()).collect();
        let mut missing = Vec::new();
        for entry in std::fs::read_dir(dir).expect("the harness directory is readable") {
            let path = entry.expect("a readable dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            let src = std::fs::read_to_string(&path).expect("a readable source file");
            let prod = production_half(&src);
            // A doc comment naming the type is not a refusal site.
            let raises = prod
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .any(|l| l.contains("HarnessError::Validation("));
            if raises && !parsed.contains(name.as_str()) {
                missing.push(name);
            }
        }
        assert!(
            missing.is_empty(),
            "these harness modules raise refusals and no HARNESS_MODULES row parses them, so the \
             published set is SHORT and nothing says so: {missing:?}. Add a row naming what each \
             one decides about a profile."
        );
    }

    /// ⚠ The declared count of identifier-forward sites matches what the parser found, both ways.
    #[test]
    fn every_indirect_refusal_site_is_declared() {
        let (_, indirect) = all_refusals();
        let mut found: BTreeMap<&str, usize> = BTreeMap::new();
        for module in indirect {
            *found.entry(module).or_default() += 1;
        }
        let declared: BTreeMap<&str, usize> =
            INDIRECT_REFUSAL_SITES.iter().map(|(m, n, _)| (*m, *n)).collect();
        assert_eq!(
            found, declared,
            "the refusal sites carrying no literal disagree with INDIRECT_REFUSAL_SITES. A site \
             that forwards a value this parser cannot read is fine — but it must be DECLARED with \
             what it forwards, or 'the parser found no message here' quietly means 'this module \
             refuses nothing'."
        );
    }

    /// Every parsed module's `owns` says something, and its path resolves.
    #[test]
    fn every_harness_module_row_is_whole() {
        for m in HARNESS_MODULES {
            assert!(m.path.starts_with("crates/"), "{} is not repo-root-relative", m.path);
            assert!(!m.owns.trim().is_empty(), "{} declares no `owns`", m.path);
            assert!(
                !production_half(m.src).is_empty(),
                "{} compiled in as an empty source — check the include_str! path",
                m.path
            );
        }
    }

    #[test]
    fn every_refusal_carries_a_readable_message() {
        for r in all_refusals().0 {
            assert!(!r.message.trim().is_empty(), "an empty refusal message in `{}`", r.on_fn);
            // ⚠ A newline is LEGAL in a message and this assertion used to forbid one: several
            // refusals lay a TOML example out over two lines on purpose (`[walkforward]\nn_splits
            // = 4`), and forbidding the character would have forced the parser to mangle them.
            // What must not survive is the ARTEFACT of an unhandled `\`-continuation, which shows
            // as a newline followed by the source's own indentation.
            assert!(
                !r.message.contains("\n  "),
                "the refusal in `{}` carries a newline followed by indentation, so a \
                 `\\`-continuation was not collapsed: {:?}",
                r.on_fn,
                r.message
            );
            assert!(
                !r.message.contains('\\'),
                "the refusal in `{}` carries a stray backslash, so an escape reached the export \
                 unprocessed: {:?}",
                r.on_fn,
                r.message
            );
        }
    }

    #[test]
    fn no_evidence_cites_a_line_number() {
        let doc = render_profile_json();
        let v: serde_json::Value = serde_json::from_str(&doc).expect("valid JSON");
        let mut stack = vec![&v];
        while let Some(node) = stack.pop() {
            match node {
                serde_json::Value::Object(m) => {
                    if let Some(serde_json::Value::String(f)) = m.get("file") {
                        assert!(
                            f.starts_with("crates/"),
                            "an evidence file is not repo-root-relative: {f:?}"
                        );
                        assert!(
                            !f.contains(':'),
                            "an evidence file cites a line number, which rots silently: {f:?}"
                        );
                    }
                    stack.extend(m.values());
                }
                serde_json::Value::Array(a) => stack.extend(a.iter()),
                _ => {}
            }
        }
    }

    /// The two facts the published page got WRONG, pinned so the export can never re-derive them.
    #[test]
    fn the_walkforward_table_declares_a_window_shape_not_one_knob() {
        let (_, w) = walked();
        let keys: BTreeSet<&str> =
            w.keys.iter().filter(|k| k.table == "walkforward").map(|k| k.key.as_str()).collect();
        for expected in ["n_splits", "train", "test", "step", "purge", "embargo"] {
            assert!(
                keys.contains(expected),
                "`walkforward.{expected}` is gone from the schema. If the key really was removed, \
                 re-render the fixture; this test exists because a published page described this \
                 table as carrying `n_splits` alone while it carried nine fields."
            );
        }
    }

    #[test]
    fn the_grid_table_is_paramscan_and_sweep_is_only_an_alias() {
        let (_, w) = walked();
        let row = w
            .keys
            .iter()
            .find(|k| k.path == "paramscan")
            .expect("the grid table is still named `paramscan` at the profile root");
        assert!(
            row.aliases.iter().any(|a| a == "sweep"),
            "`paramscan` no longer carries the `sweep` alias — every profile written before the \
             rename would stop loading, and a published page already calls the table `[sweep]`"
        );
        assert!(
            !w.keys.iter().any(|k| k.path == "sweep"),
            "there is a key literally named `sweep` — the export must publish the FIELD name and \
             the alias separately, never the alias as the key"
        );
    }

    #[test]
    fn the_rendered_document_is_well_formed() {
        let files = rendered_files();
        let doc = files.get(PROFILE_JSON).expect("profile.json is rendered");
        assert!(doc.ends_with('\n'), "the asset does not end with a newline");
        assert!(
            !doc.contains('\r'),
            "the asset carries a CR, so it was rendered from a CRLF checkout and would differ by \
             BOX — production_half must normalise before parsing"
        );
        let v: serde_json::Value = serde_json::from_str(doc).expect("the asset is valid JSON");
        for field in ["tables", "keys", "rosters", "refusals", "sizer_kinds"] {
            assert!(
                v.get(field).and_then(|f| f.as_array()).is_some_and(|a| !a.is_empty()),
                "`{field}` is missing or empty in the rendered asset"
            );
        }
    }

    #[test]
    fn the_render_equals_the_committed_fixture() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/profile.json");
        let committed = std::fs::read_to_string(path).unwrap_or_else(|e| {
            panic!(
                "the committed fixture is missing at {path} ({e}) — it is the frozen schema this \
                 export is written against"
            )
        });
        let files = rendered_files();
        let rendered = files.get(PROFILE_JSON).expect("profile.json is rendered");
        let a: serde_json::Value =
            serde_json::from_str(&committed).expect("the committed fixture is valid JSON");
        let b: serde_json::Value =
            serde_json::from_str(rendered).expect("the render is valid JSON");
        for field in ["tables", "keys", "rosters", "sizer_kinds", "refusals", "foreign_structs"] {
            assert_eq!(
                a.get(field),
                b.get(field),
                "the render and the committed fixture disagree about `{field}` — re-render the \
                 fixture with `vike-cli` / the writer, or fix the schema"
            );
        }
    }

    /// **THE WRITER** — re-render `tests/fixtures/profile.json` from the types, in place.
    ///
    /// `#[ignore]`d, so it never runs in CI or in `just test`; run it deliberately when
    /// [`the_render_equals_the_committed_fixture`] fails because the SCHEMA legitimately grew:
    ///
    /// ```sh
    /// cargo test -p vike-backtest --lib profile_surface::tests::bless -- --ignored
    /// ```
    ///
    /// Then read the diff before committing it. That review is the whole point of the fixture
    /// being committed at all: the comparison test's job is to make a schema change VISIBLE, and a
    /// writer run without reading its output converts the gate into a rubber stamp.
    ///
    /// ⚠ It exists because that failure message has always said "re-render the fixture with
    /// `vike-cli` / the writer" and there was NO writer — `vike-cli` renders `cli.json` through its
    /// own `surface::rendered_files` and has never rendered this asset, and no bin in this crate
    /// emits it either. So the only way to bless it was to hand-assemble the bytes, which is
    /// exactly how a fixture acquires a typo the gate then pins forever. This is four lines and it
    /// closes that.
    ///
    /// ⚠ **It writes into the source tree**, which is why it carries `#[ignore]` rather than an
    /// env-var guard: an `#[ignore]`d test cannot be reached by a filter that does not also pass
    /// `--ignored`, whereas a `VIKE_*` variable left set in a shell rewrites the fixture during an
    /// ordinary test run and the comparison above then passes against whatever the tree happens to
    /// render. The path is derived from `CARGO_MANIFEST_DIR`, so it writes inside THIS crate and
    /// nowhere else.
    #[test]
    #[ignore = "writes into the source tree — run it deliberately, then read the diff"]
    fn bless_the_committed_fixture() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/profile.json");
        let files = rendered_files();
        let rendered = files.get(PROFILE_JSON).expect("profile.json is rendered");
        std::fs::write(path, rendered).unwrap_or_else(|e| panic!("could not write {path}: {e}"));
        eprintln!("wrote {} bytes to {path}", rendered.len());
    }
}
