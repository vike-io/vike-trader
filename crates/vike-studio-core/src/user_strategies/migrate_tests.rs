use super::*;
use crate::user_strategies::{load_rhai_strategies, load_user_strategies, resolve_preset};
use std::assert_matches;

fn rhai(name: &str, code: &str) -> LegacyEntry {
    LegacyEntry { name: name.to_string(), body: LegacyBody::Rhai { code: code.to_string() } }
}

fn native(name: &str, registry: &str, params: &[(&str, &str)]) -> LegacyEntry {
    LegacyEntry {
        name: name.to_string(),
        body: LegacyBody::Native {
            native: registry.to_string(),
            params: params.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        },
    }
}

/// A Rhai row becomes a strategy FOLDER with a matching entry file, and the script is
/// byte-verbatim — a migration that touched the user's code would be rewriting their work.
#[test]
fn a_rhai_entry_becomes_a_folder_and_a_matching_entry_file() {
    let code = "fn on_bar() {\n  // mine\n}\n";
    let plan = plan_migration(&[rhai("sma-cross", code)]);

    assert!(plan.skipped.is_empty(), "unexpected: {:?}", plan.skipped);
    assert_eq!(plan.files.len(), 1);
    assert_eq!(
        plan.files[0].path,
        Path::new("strategies").join("rhai").join("sma-cross").join("sma-cross.rhai")
    );
    assert_eq!(plan.files[0].contents, code, "the script must be verbatim");
}

/// A Native row is NOT a strategy: the folder is the REGISTRY strategy's, the file is the
/// user's entry name, and the content is the params table. See the module doc.
#[test]
fn a_native_entry_becomes_a_preset_filed_under_its_registry_strategy() {
    let plan = plan_migration(&[native("hold-2", "buy_hold", &[("size", "2")])]);

    assert!(plan.skipped.is_empty(), "unexpected: {:?}", plan.skipped);
    assert_eq!(
        plan.files[0].path,
        Path::new("strategies").join("rust").join("buy_hold").join("hold-2.toml"),
        "folder = the registry strategy, file = the user's preset name"
    );
    let parsed: toml::Value = toml::from_str(&plan.files[0].contents).unwrap();
    assert_eq!(parsed.get("size").and_then(toml::Value::as_integer), Some(2));
    assert!(
        plan.files[0].contents.contains("buy_hold"),
        "the provenance header must name what it presets: {}",
        plan.files[0].contents
    );
}

/// AWKWARD VALUES — the reason this goes through `params_from_rows` + a TOML serialiser rather
/// than a text template. Each row below is invalid TOML *as written* or would corrupt a
/// template, and each must survive as the value the run-time path would have produced.
#[test]
fn awkward_param_values_survive_as_valid_quoted_toml() {
    let rows = [
        ("symbol", "BTCUSDT"),          // bare: not valid TOML on its own -> string
        ("note", "he said \"buy\""),    // embedded quotes -> escaped
        ("comment", "# not a comment"), // a leading '#' would eat the line in a template
        ("path", "C:\\data\\ticks"),    // backslashes -> escaped, not an escape sequence
        ("size", "2"),                  // stays an INTEGER
        ("live", "true"),               // stays a BOOL
        ("rate", "2.5"),                // stays a FLOAT
    ];
    let plan = plan_migration(&[native("awkward", "buy_hold", &rows)]);
    assert!(plan.skipped.is_empty(), "unexpected: {:?}", plan.skipped);

    let parsed: toml::Value =
        toml::from_str(&plan.files[0].contents).expect("the migrated preset must be valid TOML");

    // The load-bearing claim: the migrated file means EXACTLY what the JSON row meant at run
    // time, which is what `params_from_rows` would have made of the same text.
    let owned: Vec<(String, String)> =
        rows.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    assert_eq!(parsed, params_from_rows(&owned), "the preset must round-trip the run-time table");

    assert_eq!(parsed.get("symbol").and_then(toml::Value::as_str), Some("BTCUSDT"));
    assert_eq!(parsed.get("note").and_then(toml::Value::as_str), Some("he said \"buy\""));
    assert_eq!(parsed.get("comment").and_then(toml::Value::as_str), Some("# not a comment"));
    assert_eq!(parsed.get("path").and_then(toml::Value::as_str), Some("C:\\data\\ticks"));
    assert_eq!(parsed.get("size").and_then(toml::Value::as_integer), Some(2));
    assert_eq!(parsed.get("live").and_then(toml::Value::as_bool), Some(true));
    assert_eq!(parsed.get("rate").and_then(toml::Value::as_float), Some(2.5));
}

/// IDEMPOTENCY, in the shape that matters: the second run writes NOTHING, reports every target
/// as already present, and does not overwrite a file the user has since edited.
#[test]
fn migration_is_idempotent_and_never_overwrites() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let entries = [rhai("sma-cross", "fn on_bar() {}\n"), native("hold", "buy_hold", &[])];
    let plan = plan_migration(&entries);

    let first = apply_migration(&plan, root);
    assert_eq!(first.written.len(), 2, "{:?}", first.skipped);
    assert!(first.skipped.is_empty(), "unexpected: {:?}", first.skipped);

    // The user edits a migrated file — the case a re-run must not destroy.
    let edited = root.join("strategies").join("rhai").join("sma-cross").join("sma-cross.rhai");
    std::fs::write(&edited, "fn on_bar() { /* my edit */ }\n").unwrap();

    let second = apply_migration(&plan, root);
    assert!(second.written.is_empty(), "a re-run must write nothing: {:?}", second.written);
    assert_eq!(second.skipped.len(), 2);
    assert!(
        second.skipped.iter().all(|s| matches!(s, MigrationSkip::AlreadyPresent { .. })),
        "{:?}",
        second.skipped
    );
    assert_eq!(
        std::fs::read_to_string(&edited).unwrap(),
        "fn on_bar() { /* my edit */ }\n",
        "the user's edit must survive"
    );
}

/// A saved name is free text and was never constrained. A traversal attempt must land INSIDE
/// the root — the property `slug`'s leading/trailing strip buys structurally.
#[test]
fn a_name_that_would_escape_the_root_is_confined_to_it() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("user_data");
    let plan = plan_migration(&[
        rhai("../../secrets", "fn on_bar() {}\n"),
        rhai("BTC / ETH pairs (v2)", "fn on_bar() {}\n"),
    ]);

    for file in &plan.files {
        assert!(!file.path.is_absolute(), "{} must be relative", file.path.display());
        assert!(
            !file.path.components().any(|c| c == std::path::Component::ParentDir),
            "{} must not traverse",
            file.path.display()
        );
    }
    let out = apply_migration(&plan, &root);
    assert_eq!(out.written.len(), 2, "{:?}", out.skipped);
    for path in &out.written {
        assert!(path.starts_with(&root), "{} escaped the root", path.display());
    }
}

/// A blank name (and one made only of separators) has no filename form. Reported, never
/// silently renamed to something the user would not find again.
#[test]
fn an_unnameable_entry_is_reported_rather_than_invented() {
    let plan = plan_migration(&[rhai("   ", "fn on_bar() {}\n"), rhai("///", "x")]);
    assert!(plan.files.is_empty());
    assert_eq!(plan.skipped.len(), 2);
    assert!(plan.skipped.iter().all(|s| matches!(s, MigrationSkip::Unnameable { .. })));
    assert!(plan.skipped[0].to_string().contains("rename"), "the message must state the fix");
}

/// Two rows whose names collapse to one stem: the first wins, the second is named. Saved names
/// were never unique-constrained, so this is reachable from ordinary use.
#[test]
fn two_entries_that_would_write_the_same_file_collide_loudly() {
    let plan = plan_migration(&[
        rhai("my strat", "fn on_bar() {}\n"),
        rhai("my/strat", "fn on_bar() { /* other */ }\n"),
        rhai("MY-STRAT", "fn on_bar() { /* third */ }\n"),
    ]);

    assert_eq!(plan.files.len(), 1, "only the first may claim the path");
    assert_eq!(plan.files[0].entry, "my strat");
    assert_eq!(plan.skipped.len(), 2);
    match &plan.skipped[0] {
        MigrationSkip::Collision { entry, first, .. } => {
            assert_eq!(entry, "my/strat");
            assert_eq!(first, "my strat");
        }
        other => panic!("expected Collision, got {other:?}"),
    }
    assert_matches!(
        &plan.skipped[1], MigrationSkip::Collision { entry, .. } if entry == "MY-STRAT",
        "a case-only difference collides too: the next filesystem cannot hold both"
    );
}

/// A native row with no registry name has nothing to be a preset for.
#[test]
fn a_native_entry_without_a_registry_name_is_reported() {
    let plan = plan_migration(&[native("orphan", "", &[("size", "1")])]);
    assert!(plan.files.is_empty());
    assert_matches!(plan.skipped[0], MigrationSkip::NamelessNative { .. });
}

/// END TO END: a migrated Rhai row is a strategy the LOADER finds, compiles and reports
/// clean. Either half can be correct alone and still disagree about the layout; this is the
/// test that says they do not.
#[test]
fn a_migrated_rhai_entry_loads_back_through_the_directory_loader() {
    let tmp = tempfile::tempdir().unwrap();
    let user_data = tmp.path().join("user_data");
    let plan = plan_migration(&[rhai("sma-cross", "fn on_bar() {}\n")]);
    let out = apply_migration(&plan, &user_data);
    assert_eq!(out.written.len(), 1, "{:?}", out.skipped);

    let report = load_rhai_strategies(&user_data.join(STRATEGIES_SUBDIR).join(RHAI_SUBDIR));

    assert!(report.diagnostics.is_empty(), "unexpected: {:?}", report.diagnostics);
    assert_eq!(report.strategies.len(), 1);
    assert_eq!(report.strategies[0].name, "sma-cross");
    assert_eq!(report.strategies[0].source(), Some("fn on_bar() {}\n"));
}

/// END TO END for the OTHER population, and the claim this module's doc made in advance: a
/// migrated NATIVE row lands as a preset for a built-in strategy, the loader accepts that
/// entry-file-less folder as legitimate, and the preset RESOLVES back into the params the
/// strategy is constructed with. Before the rust-side loader existed, the same tree produced
/// one `MissingEntry` error per migrated row and nothing was resolvable at all.
#[test]
fn a_migrated_native_entry_resolves_back_as_a_preset_for_its_builtin() {
    let tmp = tempfile::tempdir().unwrap();
    let user_data = tmp.path().join("user_data");
    let plan =
        plan_migration(&[native("hold-2", "buy_hold", &[("size", "2"), ("symbol", "BTCUSDT")])]);
    let out = apply_migration(&plan, &user_data);
    assert_eq!(out.written.len(), 1, "{:?}", out.skipped);

    let report = load_user_strategies(&user_data.join(STRATEGIES_SUBDIR));

    assert!(report.diagnostics.is_empty(), "unexpected: {:?}", report.diagnostics);
    assert_eq!(report.strategies.len(), 1);
    assert!(report.strategies[0].is_native(), "the code for buy_hold is in the binary");
    let run = resolve_preset(&report, "buy_hold", "hold-2").expect("the migrated preset");
    match &run.spec {
        crate::StrategySpec::Native { name, params } => {
            assert_eq!(name, "buy_hold");
            assert_eq!(params.get("size").and_then(toml::Value::as_integer), Some(2));
            assert_eq!(params.get("symbol").and_then(toml::Value::as_str), Some("BTCUSDT"));
        }
        other => panic!("expected Native, got {other:?}"),
    }
    run.build().expect("a migrated preset builds the strategy it presets");
}

/// `slug` keeps what a filesystem accepts and drops what it does not — pinned directly,
/// because every other test in this file rests on it.
#[test]
fn slug_keeps_readable_names_and_refuses_empty_ones() {
    assert_eq!(slug("sma-cross").as_deref(), Some("sma-cross"));
    assert_eq!(slug("BTC / ETH pairs (v2)").as_deref(), Some("BTC-ETH-pairs-v2"));
    assert_eq!(slug("  padded  ").as_deref(), Some("padded"));
    assert_eq!(slug("../../secrets").as_deref(), Some("secrets"));
    assert_eq!(slug("моя-стратегия").as_deref(), Some("моя-стратегия"), "Unicode is a filename");
    assert_eq!(slug(""), None);
    assert_eq!(slug("..."), None);
    assert_eq!(slug("///"), None);
}
