//! The total source parser: structs, enums, refusals and the sizer roster, read off the text.

use super::{Enum, Field, HARNESS_MODULES, Refusal, SizerKind, Struct};

// ---------------------------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------------------------

/// Normalise an embedded source: CRLF to LF, and cut the `#[cfg(test)]` half.
///
/// The test half is cut because it quotes expected refusal substrings, and a message a test
/// asserts is not a message the binary raises.
pub(super) fn production_half(src: &str) -> String {
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
pub(super) fn parse_structs(src: &str, source: &'static str) -> Vec<Struct> {
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
pub(super) fn parse_enums(src: &str, source: &'static str) -> Vec<Enum> {
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
pub(super) fn parse_refusals(src: &str, source: &'static str) -> (Vec<Refusal>, Vec<&'static str>) {
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
            if ["fn ", "pub fn ", "pub(crate) fn ", "pub(super) fn "]
                .iter()
                .any(|h| line.starts_with(h))
            {
                cur_impl = None;
            }
        }
        let t = line.trim_start();
        for head in ["pub fn ", "pub(crate) fn ", "pub(super) fn ", "fn "] {
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
pub(super) fn parse_sizer_kinds(src: &str) -> Vec<SizerKind> {
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
pub(super) fn parse_sizer_wrapping_set(src: &str) -> Vec<String> {
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
