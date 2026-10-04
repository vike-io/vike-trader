use super::*;
use crate::cmd::backtest::resolve_addr;

/// The ladder, all three rungs. ⚠ It is NOT a copy: this asserts
/// `crate::cmd::backtest::resolve_addr`, the ONE implementation `crates/vike-cli/src/cmd/study.rs`
/// also calls, so the three verbs that dial the compute daemon cannot disagree about a blank
/// rung and aim one client at the wrong port.
#[test]
fn the_address_ladder_is_cli_then_configured_then_default() {
    assert_eq!(resolve_addr(Some("1.2.3.4:9"), Some("5.6.7.8:9")), "1.2.3.4:9");
    assert_eq!(resolve_addr(None, Some("5.6.7.8:9")), "5.6.7.8:9");
    assert_eq!(resolve_addr(None, None), vike_config::DEFAULT_BACKTEST_ADDR);
    // A blank rung is skipped, not honoured — an `Environment=` line that set nothing must not
    // send the client at an empty address.
    assert_eq!(resolve_addr(Some("  "), Some("5.6.7.8:9")), "5.6.7.8:9");
    assert_eq!(resolve_addr(Some("  "), Some("  ")), vike_config::DEFAULT_BACKTEST_ADDR);
}

#[test]
fn the_json_document_is_the_roster_and_the_address_it_came_from() {
    let doc: serde_json::Value =
        serde_json::from_str(&strategies_json("127.0.0.1:7880", &["buy_hold".to_string()]))
            .unwrap();
    assert_eq!(doc["addr"], serde_json::json!("127.0.0.1:7880"));
    assert_eq!(doc["strategies"], serde_json::json!(["buy_hold"]));
    assert_eq!(doc["count"], serde_json::json!(1));
}

/// ⚠ **Every shipped starter is reachable by a SHELL-TYPEABLE id.** The labels carry spaces, so
/// an id that was merely the label would be unusable at a prompt — and an id that failed to
/// resolve would leave a starter listed and unfetchable, which is the shape of over-advertising
/// this module exists to avoid.
#[test]
fn every_shipped_starter_resolves_by_an_id_a_shell_can_type() {
    let rows = template_rows();
    assert_eq!(rows.len(), crate::cmd::mcp::TEMPLATES.len(), "a starter is missing a row");
    assert!(!rows.is_empty(), "the const ships starters, so the roster cannot be empty");
    for (id, label, _) in &rows {
        assert!(!id.contains(' '), "`{id}` has a space in it and cannot be typed bare");
        assert!(
            !id.is_empty() && id.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
            "`{id}` is not a plain lowercase id (from the label {label:?})"
        );
        let found = find_template(id).expect("an advertised id resolves");
        assert_eq!(&found.1, label, "`{id}` resolved to a different starter");
        // ...and case-insensitively, because a reader who typed the LABEL's capitalisation
        // should not be told the starter does not exist.
        assert!(find_template(&id.to_ascii_uppercase()).is_some(), "`{id}` is case-sensitive");
    }
}

/// ⚠ The label is NOT a second spelling. Accepting it would be two names for one thing, which
/// decision 11 of the backtest-CLI-surface design refuses across this whole plane.
#[test]
fn a_starter_label_is_not_a_second_spelling_of_its_id() {
    let (_, label, _) = template_rows().into_iter().next().expect("a starter ships");
    assert!(label.contains(' '), "this test needs a multi-word label to be about anything");
    assert!(find_template(label).is_none(), "the LABEL resolved, and only the id may");
}

/// An unknown id names the roster rather than shrugging, and the roster it names is the one the
/// verb serves — `crate::cmd::params`' `the_refusal_roster_and_the_resolver_cover_the_same_names`
/// is the shape.
#[test]
fn an_unknown_starter_is_a_usage_refusal_that_names_every_id() {
    let e = run_templates(Some("no-such-starter"), None, false).expect_err("unknown");
    assert_eq!(e.exit, Exit::Usage);
    assert!(e.msg.contains("no-such-starter"), "{}", e.msg);
    for (id, _, _) in template_rows() {
        assert!(e.msg.contains(&id), "the refusal omits `{id}`: {}", e.msg);
    }
}

/// ⚠ A `--write-strategy` with no starter named is a REFUSAL, not a roster print. Printing the
/// listing and exiting 0 is indistinguishable from a write that happened.
#[test]
fn a_write_with_no_starter_named_is_refused_rather_than_listing() {
    let e = run_templates(None, Some("out.rhai"), false).expect_err("no starter");
    assert_eq!(e.exit, Exit::Usage);
    assert!(e.msg.contains("out.rhai"), "it names the path asked for: {}", e.msg);
    assert!(e.msg.contains("templates <id>"), "…and the shape that works: {}", e.msg);
}

/// The emitted source has no leading blank line and ends with exactly one newline — the two
/// properties that make `templates <id> > s.rhai` a valid script rather than an off-by-a-line
/// one.
#[test]
fn the_emitted_source_is_a_file_a_compiler_would_accept() {
    for (_, label, code) in template_rows() {
        let src = template_source(code);
        assert!(!src.starts_with('\n'), "{label} starts with a blank line");
        assert!(src.ends_with('\n'), "{label} does not end with a newline");
        assert!(!src.ends_with("\n\n"), "{label} ends with a blank line");
        // And it COMPILES — the same offline path `script-check` answers with, so the roster
        // cannot ship a starter this binary would refuse.
        vike_script::discover_params(src)
            .unwrap_or_else(|e| panic!("the shipped starter {label} does not compile: {e}"));
    }
}

/// The `--json` document carries BOTH spellings, and `name` is the agent surface's spelling.
#[test]
fn the_templates_document_carries_the_id_and_the_agent_surfaces_name() {
    let rows = template_rows();
    let doc: serde_json::Value = serde_json::from_str(&templates_json(&rows)).unwrap();
    assert_eq!(doc["count"], serde_json::json!(rows.len()));
    let first = &doc["templates"][0];
    assert_eq!(first["id"], serde_json::json!(rows[0].0));
    assert_eq!(
        first["name"],
        serde_json::json!(rows[0].1),
        "`name` must stay the LABEL — `list_templates` emits it under that key"
    );
    assert!(
        first["code"].as_str().is_some_and(|s| !s.starts_with('\n')),
        "the document carries the trimmed source, like the print does"
    );
}

/// ⚠ **THE ROWS ARE EXACTLY THE HOST-BOUND NAMES, BOTH DIRECTIONS.**
///
/// A row for a name the host does not bind advertises a call that raises on every bar and
/// self-disables the strategy; a bound name with no row is a verb the user is never told about,
/// on a release install where there is no source tree to read instead. Neither can be caught by
/// reading the table, which is why this iterates `vike_script::HOST_FN_NAMES`.
///
/// The hooks are skipped, and their own admission in [`HOST_FNS`]' doc says why: a hook is
/// DEFINED by the script rather than called, so it is not in that roster and should not be.
#[test]
fn the_rows_are_exactly_the_host_bound_names() {
    for name in vike_script::HOST_FN_NAMES {
        let rows = HOST_FNS.iter().filter(|f| f.name == *name && f.kind != HostKind::Hook).count();
        assert_eq!(
            rows, 1,
            "`{name}` is a host-bound function and this table gives it {rows} row(s) — a \
                 release install has no source tree, so a name missing here is a verb nobody is \
                 told about"
        );
    }
    for f in HOST_FNS {
        if f.kind == HostKind::Hook {
            continue;
        }
        assert!(
            vike_script::HOST_FN_NAMES.contains(&f.name),
            "`{}` is advertised as callable and the host binds no such name — a script naming \
                 it compiles, raises every bar and self-disables",
            f.name
        );
    }
}

/// Every row says what it is called, what comes back and what it means. A blank column is a
/// listing that looks complete and answers nothing.
#[test]
fn every_host_row_carries_a_call_form_a_return_and_a_note() {
    for f in HOST_FNS {
        assert!(f.call.contains(f.name), "{}'s call form does not name it: {}", f.name, f.call);
        assert!(!f.returns.is_empty(), "{} says nothing about what it returns", f.name);
        assert!(!f.note.is_empty(), "{} carries no note", f.name);
    }
    // Every kind is populated: an empty block would silently vanish from the listing.
    for kind in HOST_KIND_ORDER {
        let populated = HOST_FNS.iter().any(|f| f.kind == kind);
        assert!(populated, "the {kind:?} block is empty and prints as nothing");
    }
}

/// ⚠ The indicator half prints SPELLINGS that resolve, never registry names. `bollinger` is the
/// witness in both directions: its bare name is refused and `bollinger_mid` is callable.
#[test]
fn the_indicator_half_prints_only_spellings_that_resolve() {
    let spellings = callable_indicator_spellings();
    assert!(!spellings.is_empty(), "the host binds a catalog, so this cannot be empty");
    let mut printed = std::collections::BTreeSet::new();
    for s in &spellings {
        assert!(printed.insert(s.clone()), "`{s}` is printed twice");
    }
    // Every bare name the host binds IS printed...
    for name in vike_script::RHAI_INDICATORS.iter() {
        assert!(printed.contains(*name), "`{name}` binds bare and is not printed");
    }
    // ...and so is every per-line accessor, which is the half a bare-name roster loses:
    // `bollinger` is absent above while `bollinger_mid` is callable.
    for m in vike_indicators::registry() {
        for (_, call) in vike_script::line_accessors(m.name) {
            assert!(printed.contains(&call), "`{call}` is callable and is not printed");
        }
    }
    // ⚠ A registry name reachable by NO spelling must not be printed. `is_callable` is the
    // authority for that question, so this asks it rather than naming a witness that could
    // later become bound.
    for m in vike_indicators::registry() {
        if !vike_script::is_callable(m.name) {
            assert!(!printed.contains(m.name), "`{}` is unreachable and is printed", m.name);
        }
    }
}

/// The rendering carries every block heading and every call form, so a reader is not handed a
/// listing with a family missing.
#[test]
fn the_rendering_carries_every_block_and_every_call_form() {
    // ONE derivation, bound once: the user-indicator half reads a process-wide `OnceLock`, so
    // asking twice in one test is a way to compare two different answers.
    let spellings = callable_indicator_spellings();
    let text = render_host_api(&spellings);
    for kind in HOST_KIND_ORDER {
        assert!(text.contains(kind.heading()), "the listing omits the {kind:?} block");
    }
    for f in HOST_FNS {
        assert!(text.contains(f.call), "the listing omits `{}`", f.call);
    }
    // The three language-level facts a roster cannot carry.
    assert!(text.contains("vike-cli indicators"), "it does not say where the detail lives");
    assert!(text.contains("import"), "it does not say a script cannot read a file");
    assert!(text.contains("stdout"), "it does not say where print goes");
    // No number is written down: the count is rendered from the derived list.
    assert!(
        text.contains(&format!("{} callable", spellings.len())),
        "the count must be the derived one"
    );
}

/// ⚠ The `--json` document's `kind` is the discriminator a consumer branches on, so every value
/// it can carry is asserted to be one of the four.
#[test]
fn the_host_api_document_discriminates_by_kind() {
    let doc: serde_json::Value =
        serde_json::from_str(&host_api_json(&["sma".to_string()])).unwrap();
    let rows = doc["functions"].as_array().expect("an array");
    assert_eq!(rows.len(), HOST_FNS.len());
    for row in rows {
        let kind = row["kind"].as_str().expect("a string");
        assert!(
            ["hook", "read", "order", "knob"].contains(&kind),
            "the document carries the unknown kind {kind:?}"
        );
    }
    assert_eq!(doc["indicators"]["count"], serde_json::json!(1));
    assert_eq!(doc["indicators"]["callable"], serde_json::json!(["sma"]));
}

/// A missing `--script` is the ONE rung-2 this verb produces: the command line is what has to
/// change, and nothing was compiled.
#[test]
fn script_check_without_a_script_is_a_usage_refusal() {
    let e = run_script_check(None, false).expect_err("no script");
    assert_eq!(e.exit, Exit::Usage);
    assert!(e.msg.contains("--script"), "{}", e.msg);
    // A blank value is treated as absent rather than as a path named `""`, which would report
    // an io error about nothing.
    assert_eq!(run_script_check(Some("   "), false).expect_err("blank").exit, Exit::Usage);
}

/// ⚠ **A compile error is `Failed` (1), not `Usage` (2) and not `Breach` (6)** — the rung
/// argument is on [`run_script_check`], and this is what would catch it being re-classified.
/// One compile error, one number, shared with `backtest params --script`.
#[test]
fn a_bad_script_answers_on_the_failed_rung_with_the_diagnostic_in_the_document() {
    // ⚠ `tempfile::TempDir`, bound for the whole scope — the idiom
    // `crate::cmd::node::connect`'s own scratch helper argues: a guard dropped at the end of
    // the statement deletes the directory before the file under test is opened.
    let dir = tempfile::Builder::new()
        .prefix("vike-cli-script-check")
        .tempdir()
        .expect("a scratch directory");
    let bad = dir.path().join("bad.rhai");
    std::fs::write(&bad, "fn on_bar( {").expect("write");
    let path = bad.to_string_lossy().to_string();

    let rung = run_script_check(Some(&path), true).expect("a verdict, not a failure");
    assert_eq!(rung, Exit::Failed, "a script that does not compile is the Failed rung");

    // The DOCUMENT is what carries the diagnostic — the reason this verb prints to stdout and
    // returns a rung instead of routing through `CliError`.
    let doc: serde_json::Value = serde_json::from_str(&check_json(&path, Some("boom"), 0))
        .expect("the document is valid JSON");
    assert_eq!(doc["ok"], serde_json::json!(false));
    assert_eq!(doc["error"], serde_json::json!("boom"));
    assert_eq!(doc["script"], serde_json::json!(path));

    std::fs::write(&bad, "let n = param(\"n\", 3.0);\nfn on_bar() { }\n").expect("write");
    assert_eq!(
        run_script_check(Some(&path), false).expect("a verdict"),
        Exit::Ok,
        "a script that compiles is the Ok rung"
    );
}

/// An unreadable file is NOT a verdict about a script: nothing was compiled, so there is no
/// diagnostic to print and the failure goes through `CliError` like every other io failure.
#[test]
fn an_unreadable_script_is_a_failure_rather_than_a_verdict() {
    let e = run_script_check(Some("no-such-file-for-script-check.rhai"), false)
        .expect_err("unreadable");
    assert_eq!(e.exit, Exit::Failed);
    assert!(e.msg.contains("cannot read script"), "{}", e.msg);
}

/// ⚠ The `ok`/`error` keys are the MCP tool's, byte for byte, because the two surfaces answer
/// one question off one compile path.
#[test]
fn the_check_document_spells_ok_and_error_the_way_the_agent_surface_does() {
    let doc: serde_json::Value = serde_json::from_str(&check_json("s.rhai", None, 2)).unwrap();
    assert_eq!(doc["ok"], serde_json::json!(true));
    assert_eq!(doc["error"], serde_json::Value::Null);
    assert_eq!(doc["params"], serde_json::json!(2), "the COUNT, never the list");
}

/// The wrapper keeps every name and adds no separator a reader would mistake for one.
#[test]
fn the_name_wrapper_loses_nothing() {
    let names: Vec<String> = (0..40).map(|i| format!("name_{i}")).collect();
    let text = wrap_names(&names, 40, "  ");
    for n in &names {
        assert!(text.contains(n), "the wrapper dropped `{n}`");
    }
    assert!(!text.contains(",,"), "a doubled separator: {text}");
    assert!(text.lines().all(|l| l.starts_with("  ")), "every line is indented: {text}");
    assert!(!text.trim_end().ends_with(','), "the last name carries no trailing comma");
}
