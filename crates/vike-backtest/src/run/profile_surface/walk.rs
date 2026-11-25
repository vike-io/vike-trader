//! The walk from `ROOT_STRUCT`: the `schemars` schema's properties become keys and table paths, in
//! the order serde declares the fields (`super::profile_tables`).

use std::collections::{BTreeMap, VecDeque};

use serde_json::Value;

use super::{DEFAULT_SPELLINGS, Key, ProfileTable, ROOT_STRUCT, Skipped, Table, Walked};

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
        ("integer", Some("uint64")) => "u64".to_string(),
        ("integer", Some("uint32")) => "u32".to_string(),
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

/// The first paragraph of a doc text, as one line, with intra-doc link brackets unwrapped: the
/// published `short` and a skipped field's `why`.
fn first_paragraph(doc: &str) -> String {
    let first = doc.split("\n\n").next().unwrap_or_default();
    unwrap_links(&first.replace('\n', " "))
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

pub(super) fn walk(schema: &Value, structs: &BTreeMap<String, ProfileTable>) -> Walked {
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
    // A type the walk would nest under: a registered table, or a struct a row is missing for.
    let is_table = |name: &str, key_path: &str| -> bool {
        if structs.contains_key(name) {
            return true;
        }
        assert!(
            schema["$defs"][name]["properties"].is_null(),
            "profile_surface: `{key_path}` is the table `{name}`, which no `profile_tables` row names"
        );
        false
    };

    while let Some((path, sname, ancestry, required, aot)) = queue.pop_front() {
        let s = structs.get(&sname).unwrap_or_else(|| {
            panic!(
                "profile_surface: the walk reached `{sname}`, which no `profile_tables` row names"
            )
        });
        let declared = if sname == ROOT_STRUCT { schema } else { &schema["$defs"][&sname] };
        let properties = declared["properties"].as_object().unwrap_or_else(|| {
            panic!("profile_surface: the schema of `{sname}` has no properties")
        });
        tables.push(Table {
            table: path.clone(),
            strukt: sname.clone(),
            closed: declared["additionalProperties"] == false,
            array_of_tables: aot,
            required,
            source: s.file,
        });
        for row in declared["x-skipped-fields"].as_array().into_iter().flatten() {
            skipped.push(Skipped {
                strukt: sname.clone(),
                field: row["field"].as_str().unwrap_or_default().to_string(),
                doc: first_paragraph(row["why"].as_str().unwrap_or_default()),
            });
        }
        // Nothing the schema publishes may sit outside serde's list, which gives the keys their order.
        assert_eq!(
            s.fields.len(),
            properties.len(),
            "profile_surface: `{sname}`'s schema and serde's field list disagree: {:?}",
            s.fields
        );

        for names in &s.fields {
            // Of one field's names, the schema carries exactly one; the others are its aliases.
            let mut in_schema = names.iter().filter(|name| properties.contains_key(**name));
            let (Some(wire_name), None) = (in_schema.next(), in_schema.next()) else {
                panic!(
                    "profile_surface: serde reads {names:?} into one field of `{sname}`, and its \
                     schema carries not exactly one of them"
                );
            };
            let wire_name = (*wire_name).to_string();
            let aliases: Vec<String> = names
                .iter()
                .filter(|name| **name != wire_name)
                .map(|name| name.to_string())
                .collect();
            let property = &properties[&wire_name];

            let mut ty = rust_type(property);
            let (bare, nullable) = unwrap_type(&ty);
            let key_path =
                if path.is_empty() { wire_name.clone() } else { format!("{path}.{wire_name}") };

            // What kind of TOML value is it?
            let (shape, nested, child_aot) =
                if let Some(inner) = bare.strip_prefix("Vec<").and_then(|r| r.strip_suffix('>')) {
                    let (elem, _) = unwrap_type(inner);
                    if is_table(&elem, &key_path) {
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
                } else if is_table(&bare, &key_path) {
                    ("table", Some(bare.clone()), false)
                } else {
                    ("scalar", None, false)
                };

            let recursive =
                nested.as_deref().map(|n| ancestry.iter().any(|a| a == n)).unwrap_or(false);
            let nested_table = nested.as_ref().map(|_| key_path.clone());
            if recursive {
                // A schema `$ref` cannot say `Box`, and a field that re-enters its own struct must
                // be one: Rust refuses the type otherwise.
                ty = ty.replace(&bare, &format!("Box<{bare}>"));
            }

            let required_here = declared["required"]
                .as_array()
                .is_some_and(|names| names.iter().any(|name| name == wire_name.as_str()));
            let default_fn =
                property.get("x-default-function").and_then(Value::as_str).map(str::to_string);
            // An absent key serde does not refuse reads as a default: the named function's, or the
            // type's (an `Option`'s `None` whether or not the field says `#[serde(default)]`).
            let default_kind = match (&default_fn, required_here) {
                (Some(_), _) => "named",
                (None, true) => "none",
                (None, false) => "implicit",
            };
            let default_value =
                default_fn.as_deref().and_then(|f| named_default_value(property, f));
            let description = property.get("description").and_then(Value::as_str);

            keys.push(Key {
                table: path.clone(),
                key: wire_name.clone(),
                path: key_path.clone(),
                ty,
                required: required_here,
                nullable,
                shape,
                default_kind,
                default_fn,
                default_value,
                aliases,
                nested_struct: nested.clone(),
                nested_table: nested_table.clone(),
                recursive,
                doc: first_paragraph(description.unwrap_or_default()),
                strukt: sname.clone(),
                source: s.file,
            });

            if let Some(child) = nested
                && !recursive
            {
                let mut anc = ancestry.clone();
                anc.push(child.clone());
                queue.push_back((key_path, child, anc, required_here, child_aot));
            }
        }
    }
    Walked { tables, keys, skipped }
}
