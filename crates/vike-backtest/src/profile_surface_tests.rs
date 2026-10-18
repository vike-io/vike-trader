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
    let open: Vec<&str> = w.tables.iter().filter(|t| !t.closed).map(|t| t.table.as_str()).collect();
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
/// This walks the real source folders rather than a list, because the defect it
/// exists for was an ABSENCE: the export parsed `profile.rs` alone and published its 62
/// refusals as "what the profile refuses", while four more about the same `[walkforward]`
/// table sat in `windows.rs` — and the docs page then told a reader that nothing refused a
/// `step` shorter than `test`. Nothing could have noticed, because a short list looks exactly
/// like a complete one.
///
/// ⚠ **The folders are DERIVED from the rows, never written down**: `src/harness` plus the folder
/// of every `HARNESS_MODULES` row's file (`src/search`, `src/walkforward`, …), and each folder's
/// module root beside it (`src/search.rs`). A refusing module moved out of `src/harness` took its
/// siblings out of a walk that named `src/harness` alone, so a NEW refusal raised beside it would
/// have escaped this check the same way `windows.rs` once escaped the export.
#[test]
fn every_harness_module_that_refuses_is_parsed() {
    let krate = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let parsed: BTreeSet<&str> = HARNESS_MODULES.iter().map(|m| m.path).collect();
    let mut missing = Vec::new();
    let mut roster: BTreeSet<std::path::PathBuf> = HARNESS_MODULES
        .iter()
        .map(|m| {
            let rel = m.path.strip_prefix("crates/vike-backtest/").expect("a row names this crate");
            krate.join(rel).parent().expect("a row's file sits in a folder").to_path_buf()
        })
        .collect();
    roster.insert(krate.join("src/harness"));
    // A folder inside another roster folder is already walked by that folder's recursion.
    let roster: Vec<std::path::PathBuf> = roster
        .iter()
        .filter(|d| !roster.iter().any(|o| o != *d && d.starts_with(o)))
        .cloned()
        .collect();
    // A harness module's `#[cfg(test)] mod NAME;` file is its tests, not a module that decides
    // anything about a profile — the same code `production_half` cuts when the tests sit inline.
    let test_files: BTreeSet<std::path::PathBuf> =
        roster.iter().flat_map(|d| vike_model::libm_walk::cfg_test_module_files_under(d)).collect();
    // ⚠ RECURSIVE, and keyed on the whole path: a module's children live in a folder of its name
    // (`harness/profile/*.rs`), and a flat walk by basename would never see a refusal raised there.
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    for d in &roster {
        let root = d.with_extension("rs");
        if root.is_file() {
            files.push(root);
        }
        dirs.push(d.clone());
    }
    while let Some(d) = dirs.pop() {
        for entry in std::fs::read_dir(&d).expect("a roster folder is readable") {
            let path = entry.expect("a readable dir entry").path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                files.push(path);
            }
        }
    }
    // A test module's own children (`profile/tests/*.rs` under `profile/tests.rs`) are tests too.
    let in_tests =
        |p: &std::path::Path| test_files.iter().any(|t| p.starts_with(t.with_extension("")));
    for path in files {
        if path.extension().and_then(|e| e.to_str()) != Some("rs")
            || test_files.contains(&path)
            || in_tests(&path)
        {
            continue;
        }
        let rel = path.strip_prefix(krate).expect("under the crate").to_string_lossy().to_string();
        let name = format!("crates/vike-backtest/{}", rel.replace('\\', "/"));
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
    let b: serde_json::Value = serde_json::from_str(rendered).expect("the render is valid JSON");
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
/// cargo test -p vike-backtest --lib profile_surface::profile_surface_tests::bless_the_committed_fixture -- --ignored
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
