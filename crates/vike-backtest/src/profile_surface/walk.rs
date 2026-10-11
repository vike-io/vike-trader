//! The walk from `ROOT_STRUCT`: parsed struct fields become table paths and keys.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use super::render::profile_src;
use super::{FOREIGN_STRUCTS, Key, ROOT_STRUCT, Skipped, Struct, Table, Walked, resolved_default};

// ---------------------------------------------------------------------------------------------
// The walk: struct fields become table paths
// ---------------------------------------------------------------------------------------------

/// Strip `Option<…>` / `Box<…>` wrappers, reporting whether the field was nullable.
pub(super) fn unwrap_type(ty: &str) -> (String, bool) {
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

pub(super) fn walk(structs: &BTreeMap<String, Struct>) -> Walked {
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
