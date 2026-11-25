use super::*;

fn walked() -> (BTreeMap<String, ProfileTable>, Walked) {
    let structs = profile_tables();
    let w = walk(&profile_schema(), &structs);
    (structs, w)
}

/// The repo root, which the checks that hold a hand-written fact to its source read from.
fn repo_file(file: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(file);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{file} is not readable: {e}"))
        .replace("\r\n", "\n")
}

/// The body of `pub struct {name}` in one source: its lines up to the closing brace.
fn struct_body<'a>(src: &'a str, name: &str, file: &str) -> &'a str {
    let open = format!("pub struct {name} {{\n");
    let at = src.find(&open).unwrap_or_else(|| panic!("{file} declares no `pub struct {name}`"));
    let body = &src[at + open.len()..];
    &body[..body.find("\n}\n").unwrap_or(body.len())]
}

#[test]
fn every_table_has_fields_and_the_root_is_one() {
    let structs = profile_tables();
    assert!(structs.contains_key(ROOT_STRUCT), "the root struct `{ROOT_STRUCT}` has no row");
    for (name, s) in &structs {
        assert!(!s.fields.is_empty(), "serde lists no field of `{name}`");
    }
}

/// A row is the file that declares its struct, so the evidence a key carries points at the struct.
#[test]
fn every_table_row_names_the_file_declaring_its_struct() {
    let rosters = profile_rosters();
    let rows = profile_tables().into_iter().map(|(name, s)| (name, s.file, "struct"));
    for (name, file, item) in rows.chain(rosters.iter().map(|r| (r.name.clone(), r.file, "enum"))) {
        assert!(file.starts_with("crates/"), "{file} is not repo-root-relative");
        assert!(
            repo_file(file).contains(&format!("pub {item} {name} {{")),
            "{file} does not declare `pub {item} {name}`"
        );
    }
}

#[test]
fn every_table_row_is_reached_by_the_walk() {
    let (structs, w) = walked();
    let reached: BTreeSet<&str> = w.tables.iter().map(|t| t.strukt.as_str()).collect();
    let orphans: Vec<&str> =
        structs.keys().map(String::as_str).filter(|n| !reached.contains(n)).collect();
    assert!(
        orphans.is_empty(),
        "these `profile_tables` rows are reached by no profile field, so the export publishes their \
             keys under no table: {orphans:?} — wire them to a field, or delete the row"
    );
}

/// ⚠ **What neither derive can say is declared beside the field, and held to it here**: every
/// `#[serde(default = "f")]` of a table's struct carries `x-default-function = "f"`, and every
/// `#[serde(skip)]` field is a row of its struct's `x-skipped-fields`, whose `why` is that field's
/// first doc paragraph. Read from the tree at test time: the product never reads a source.
#[test]
fn every_serde_fact_the_derives_cannot_say_is_declared_beside_it() {
    let (structs, w) = walked();
    let schema = profile_schema();
    let (mut named, mut skipped) = (BTreeSet::new(), BTreeSet::new());
    for (name, s) in &structs {
        let src = repo_file(s.file);
        let mut doc: Vec<&str> = Vec::new();
        let mut skip = false;
        for line in struct_body(&src, name, s.file).lines().map(str::trim) {
            if let Some(text) = line.strip_prefix("///") {
                doc.push(text.strip_prefix(' ').unwrap_or(text));
            } else if line == "#[serde(skip)]" {
                skip = true;
            } else if let Some(f) = line.strip_prefix("#[serde(default = \"") {
                named.insert((name.clone(), f.trim_end_matches("\")]").to_string()));
            } else if let Some(field) = line.strip_prefix("pub ").and_then(|l| l.split_once(':')) {
                if skip {
                    let first = doc.iter().take_while(|l| !l.is_empty()).copied();
                    skipped.insert((
                        name.clone(),
                        field.0.to_string(),
                        first.collect::<Vec<_>>().join(" "),
                    ));
                }
                (doc, skip) = (Vec::new(), false);
            }
        }
    }
    let published_named: BTreeSet<(String, String)> =
        w.keys.iter().filter_map(|k| Some((k.strukt.clone(), k.default_fn.clone()?))).collect();
    assert_eq!(published_named, named, "a named serde default without its `x-default-function`");
    let declared_skips: BTreeSet<(String, String, String)> = structs
        .keys()
        .flat_map(|name| {
            let def = if name == ROOT_STRUCT { &schema } else { &schema["$defs"][name] };
            let rows = def["x-skipped-fields"].as_array().cloned().unwrap_or_default();
            rows.into_iter().map(move |row| {
                let text = |key: &str| row[key].as_str().unwrap_or_default().to_string();
                (name.clone(), text("field"), text("why"))
            })
        })
        .collect();
    assert_eq!(
        declared_skips, skipped,
        "a `#[serde(skip)]` field and its struct's `x-skipped-fields` disagree (the `why` is the \
         field's first doc paragraph, verbatim)"
    );
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
    let open: Vec<&str> = w.tables.iter().filter(|t| !t.closed).map(|t| t.table.as_str()).collect();
    assert!(
        open.is_empty(),
        "these tables do not carry `deny_unknown_fields`, so a typo in them is a SILENT \
             no-op: {open:?} — the published claim that a typo is a hard load error would be false"
    );
}

/// The published `required` is what the deserializer does: an empty `[risk]` table loads, its bare
/// `Option`s included, while `[strategy]` without its `name` does not. The schema's word for it is
/// checked against a real load, not trusted.
#[test]
fn a_published_required_key_is_what_the_loader_requires() {
    let (_, w) = walked();
    let in_risk: Vec<&Key> = w.keys.iter().filter(|k| k.table == "risk").collect();
    assert!(!in_risk.is_empty() && in_risk.iter().all(|k| !k.required));
    assert!(w.keys.iter().any(|k| k.path == "strategy.file" && k.required && !k.nullable));
    toml::from_str::<vike_model::ProfileRisk>("").expect("no key of [risk] is required");
    assert!(
        toml::from_str::<crate::run::profile::ProfileStrategy>("").is_err(),
        "[strategy] loads with no `file`, so `strategy.file` is not required"
    );
}

#[test]
fn every_named_default_has_a_value() {
    let (_, w) = walked();
    for k in &w.keys {
        if k.default_kind == "named" {
            let f = k.default_fn.as_deref().expect("a named default names its function");
            assert!(
                k.default_value.is_some(),
                "`{}`'s default comes from `{f}`, and the schema carries no value for it: does its \
                     type implement `Serialize`?",
                k.path
            );
        }
    }
}

/// `DEFAULT_SPELLINGS` is an exception to reading the value off the schema: a row names a function a
/// field still calls, and a value its JSON really does not reproduce.
#[test]
fn no_default_spelling_is_stale() {
    let (_, w) = walked();
    let schema = profile_schema();
    for (name, spelled) in DEFAULT_SPELLINGS {
        let key =
            w.keys.iter().find(|k| k.default_fn.as_deref() == Some(*name)).unwrap_or_else(|| {
                panic!("no field's default names `{name}` any more: delete the row")
            });
        let table = schema["$defs"]
            .as_object()
            .and_then(|defs| defs.get(&key.strukt))
            .expect("the key's struct is in the schema");
        let json = table["properties"][&key.key]["default"].to_string();
        assert_ne!(*spelled, json, "`{name}`: the JSON reproduces it, delete the row");
    }
}

#[test]
fn every_refusal_carries_a_readable_message() {
    for (message, id, site) in all_refusals() {
        assert!(!message.trim().is_empty(), "an empty refusal message: `{id}`");
        // ⚠ A newline is LEGAL in a message (several lay a TOML example out over two lines); what
        // must not survive is a newline followed by the source's indentation, a literal broken over
        // two lines without its `\`-continuation.
        assert!(
            !message.contains("\n  "),
            "the refusal `{id}` (raised in `{}`) carries a newline followed by indentation: \
             {message:?}",
            site.on_fn
        );
    }
}

/// ⚠ **Every published refusal's site is where that refusal is raised**: the file a row names
/// constructs that variant (`ProfileRefusal::X`, `CostRefusal::X`) and declares the function the row
/// names, so `refusal_sites` cannot keep a row for a refusal that moved. Read from the tree at test
/// time: the product never reads a source to list a refusal, only this check does.
#[test]
fn every_refusal_site_names_its_variant() {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut wrong = Vec::new();
    let mut check = |variant: String, sites: &[refusal_sites::Site]| {
        for site in sites {
            let src = std::fs::read_to_string(repo.join(site.file))
                .unwrap_or_else(|e| panic!("{} is not readable: {e}", site.file));
            let constructs = src.match_indices(&variant).any(|(at, _)| {
                !src[at + variant.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
            });
            if !constructs || !src.contains(&format!("fn {}", site.on_fn)) {
                wrong.push(format!("{variant} at {}'s `{}`", site.file, site.on_fn));
            }
        }
    };
    for id in crate::run::refusals::ProfileRefusalId::ALL {
        if let refusal_sites::Raised::At(sites) = refusal_sites::profile_raised_at(id) {
            check(format!("ProfileRefusal::{id:?}"), sites);
        }
    }
    for id in vike_fills::refusals::CostRefusalId::ALL {
        check(format!("CostRefusal::{id:?}"), refusal_sites::cost_raised_at(id));
    }
    assert!(wrong.is_empty(), "these refusal sites do not raise their refusal: {wrong:#?}");
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
    for expected in
        ["n_splits", "train", "test", "step", "purge", "embargo", "groups", "test_groups"]
    {
        assert!(
            keys.contains(expected),
            "`walkforward.{expected}` is gone from the schema. If the key really was removed, \
                 re-render the fixture; this test exists because a published page described this \
                 table as carrying `n_splits` alone while it carried nine fields."
        );
    }
}

/// `[permutation]` is published whole, with its one required key.
#[test]
fn the_permutation_table_is_published_with_its_required_count() {
    let (_, w) = walked();
    let keys: Vec<&Key> = w.keys.iter().filter(|k| k.table == "permutation").collect();
    let names: BTreeSet<&str> = keys.iter().map(|k| k.key.as_str()).collect();
    assert_eq!(names, BTreeSet::from(["permutations", "seed", "train", "rank_by"]));
    let required: Vec<&str> = keys.iter().filter(|k| k.required).map(|k| k.key.as_str()).collect();
    assert_eq!(required, ["permutations"]);
}

#[test]
fn the_grid_table_is_paramscan_and_carries_no_alias() {
    let (_, w) = walked();
    let row = w
        .keys
        .iter()
        .find(|k| k.path == "paramscan")
        .expect("the grid table is still named `paramscan` at the profile root");
    assert!(
        row.aliases.is_empty(),
        "`paramscan` carries an alias again ({:?}): the owner's ruling 20 removed `sweep`, one word \
             for one thing",
        row.aliases
    );
    assert!(
        !w.keys.iter().any(|k| k.path == "sweep"),
        "there is a key literally named `sweep`: the profile has one grid table, `paramscan`"
    );
}

#[test]
fn the_rendered_document_is_well_formed() {
    let files = rendered_files();
    let doc = files.get(PROFILE_JSON).expect("profile.json is rendered");
    assert!(doc.ends_with('\n'), "the asset does not end with a newline");
    assert!(
        !doc.contains('\r'),
        "the asset carries a CR, so a CRLF checkout reached the published text and it would \
             differ by BOX"
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
    let b: serde_json::Value = serde_json::from_str(rendered).expect("the render is valid JSON");
    let fixture = a.as_object().expect("the fixture is one JSON object");
    let render = b.as_object().expect("the render is one JSON object");
    for (field, want) in fixture {
        assert_eq!(
            Some(want),
            render.get(field),
            "the render and the committed fixture disagree about `{field}` — re-render the \
                 fixture with `vike-cli` / the writer, or fix the schema"
        );
    }
    // Ruling 10: a released field is never renamed or dropped, so the document holds no key the
    // fixture does not, until a bless adds one on purpose.
    assert_eq!(
        fixture.keys().collect::<BTreeSet<_>>(),
        render.keys().collect::<BTreeSet<_>>(),
        "the render and the committed fixture carry different top-level keys"
    );
}

/// **THE WRITER** — re-render `tests/fixtures/profile.json` from the types, in place.
///
/// `#[ignore]`d, so it never runs in CI or in `just test`; run it deliberately when
/// [`the_render_equals_the_committed_fixture`] fails because the SCHEMA legitimately grew:
///
/// ```sh
/// cargo test -p vike-backtest --lib run::profile_surface::profile_surface_tests::bless_the_committed_fixture -- --ignored
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
