//! The walk from `ROOT_STRUCT`: the `schemars` schema's properties become keys and table paths, in
//! the order the source declares them.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde_json::Value;

use super::{DEFAULT_SPELLINGS, FOREIGN_STRUCTS, Key, ROOT_STRUCT, Skipped, Struct, Table, Walked};

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

/// The Rust spelling of a property's schema: the published `type` is the type the field declares,
/// and a schema says it in other words. ⚠ TOTAL: a schema shape not listed here panics, so a field of
/// a new type fails the gate instead of publishing a wrong word.
fn rust_type(property: &Value) -> String {
    if let Some(reference) = property.get("$ref").and_then(Value::as_str) {
        return reference.rsplit('/').next().expect("rsplit yields one item").to_string();
    }
    if let Some(any_of) = property.get("anyOf").and_then(Value::as_array) {
        return match any_of.as_slice() {
            [inner, null] if null["type"] == "null" => format!("Option<{}>", rust_type(inner)),
            _ => panic!("profile_surface: a schema this walk does not model: {property}"),
        };
    }
    let (word, nullable) = match &property["type"] {
        Value::String(word) => (word.as_str(), false),
        Value::Array(words) if words.len() == 2 && words[1] == "null" => {
            (words[0].as_str().unwrap_or_default(), true)
        }
        _ => panic!("profile_surface: a schema this walk does not model: {property}"),
    };
    let bare = match (word, property.get("format").and_then(Value::as_str)) {
        ("number", Some("double")) => "f64".to_string(),
        ("integer", Some("int64")) => "i64".to_string(),
        ("integer", Some("uint")) => "usize".to_string(),
        ("boolean", None) => "bool".to_string(),
        ("string", None) => "String".to_string(),
        ("array", None) => format!("Vec<{}>", rust_type(&property["items"])),
        ("object", None) if property["additionalProperties"].is_object() => {
            format!("BTreeMap<String, {}>", rust_type(&property["additionalProperties"]))
        }
        _ => panic!("profile_surface: a schema this walk does not model: {property}"),
    };
    if nullable { format!("Option<{bare}>") } else { bare }
}

/// The first paragraph of a schema `description`, as one line, as the published `short` has it.
fn first_paragraph(property: &Value) -> String {
    let description = property.get("description").and_then(Value::as_str).unwrap_or_default();
    let first = description.split("\n\n").next().unwrap_or_default();
    super::parse::unwrap_links(&first.replace('\n', " "))
}

/// The value a named `#[serde(default = "fn")]` produces, which the derive read by calling the
/// function: its JSON, or the source's own spelling where the two differ.
fn named_default_value(property: &Value, default_fn: &str) -> Option<String> {
    if let Some((_, spelled)) = DEFAULT_SPELLINGS.iter().find(|(name, _)| *name == default_fn) {
        return Some((*spelled).to_string());
    }
    match property.get("default")? {
        Value::String(text) => Some(text.clone()),
        other => Some(other.to_string()),
    }
}

pub(super) fn walk(schema: &Value, structs: &BTreeMap<String, Struct>) -> Walked {
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
        let declared = if sname == ROOT_STRUCT { schema } else { &schema["$defs"][&sname] };
        // `[risk]` has no derive (`super::RiskTable` says why): its keys are the source's.
        let from_source = sname == vike_model::risk::surface::RISK_STRUCT;
        tables.push(Table {
            table: path.clone(),
            strukt: s.name.clone(),
            closed: if from_source { s.closed } else { declared["additionalProperties"] == false },
            array_of_tables: aot,
            required,
            source: s.source,
        });

        let mut seen: BTreeSet<String> = BTreeSet::new();
        for f in &s.fields {
            let mut parts: Vec<&str> = Vec::new();
            for a in &f.serde_attributes {
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
                } else if p.starts_with("skip_serializing_if = ") {
                    // Serialize-only (a table that also rides a wire, `CsvBarsFormatWords`): it
                    // changes neither the key a profile writes nor what an absent key reads as.
                } else {
                    panic!(
                        "profile_surface: `{}::{}` carries the serde part `{p}`, which this walk \
                         does not model — it may change the wire key or its default",
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

            let property = &declared["properties"][&wire_name];
            assert!(
                from_source || property.is_object(),
                "profile_surface: the source declares `{}::{}` and the schema has no `{wire_name}`",
                s.name,
                f.name
            );
            seen.insert(wire_name.clone());

            let mut ty = if from_source { f.ty.clone() } else { rust_type(property) };
            let (bare, nullable) = unwrap_type(&ty);
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
                } else if bare == "Params" {
                    // `vike_model::params::Params` — the strategy-parameter bag, a table whose
                    // keys belong to the strategy and not to this schema.
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
            if recursive {
                // A schema `$ref` cannot say `Box`, and a field that re-enters its own struct must
                // be one: Rust refuses the type otherwise.
                ty = ty.replace(&bare, &format!("Box<{bare}>"));
            }

            let default_value =
                default_fn.as_deref().and_then(|f| named_default_value(property, f));
            // serde defaults an absent `Option` to `None` on its own, so in the source's words only a
            // non-Option field with no declared default is required.
            let required_here = if from_source {
                default_kind == "none" && !nullable
            } else {
                declared["required"]
                    .as_array()
                    .is_some_and(|names| names.iter().any(|name| name == wire_name.as_str()))
            };

            keys.push(Key {
                table: path.clone(),
                key: wire_name.clone(),
                path: key_path.clone(),
                ty,
                required: required_here,
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
                doc: if from_source { f.doc.clone() } else { first_paragraph(property) },
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
        // The other direction: nothing the schema publishes may sit outside the declaration scan
        // that gives the keys their order, or it would silently drop out of the array.
        let in_schema = declared["properties"].as_object();
        assert!(
            from_source || in_schema.is_some(),
            "profile_surface: the schema of `{sname}` has no properties"
        );
        for name in in_schema.into_iter().flat_map(serde_json::Map::keys) {
            assert!(
                seen.contains(name.as_str()),
                "profile_surface: the schema of `{sname}` has `{name}`, which the source scan did \
                 not read"
            );
        }
    }
    Walked { tables, keys, skipped }
}
