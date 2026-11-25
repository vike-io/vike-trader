//! `deploy/row-steps`, the release's row steps, held to this build's `clap` tree: every step is a `vike-cli` command line
//! it parses, every undo one the OLDER build parses, and no value holds a blank.
//!
//! The list names the box's own accounts and is not on the public mirror, so it is read at RUN time
//! (`crates/vike-core/tests/common/latency_line.rs`'s `ci_yml` is the shape) and the test skips, loudly, where it is
//! absent. `deploy/docker/vikectl`'s `parse_row_steps` reads it: a line's words split on blanks, nothing quoted or
//! evaluated, a `#` line skipped, `undo <words>` the undo of the step above it.
//!
//! An undo runs with the OLDER build's `vike-cli`, so it may spell a word this build retired. Such an undo is proved
//! by this build's own refusal table: the retired spelling refuses here BY NAME
//! (`crate::cmd::config::retired_spelling`), which is what says the older build had it. A word this build neither
//! parses nor retired is no verb of either build.

use std::path::Path;

use vike_utility::cli::check_examples;

use super::examples_tests::judging_tree;

/// The list, or `None` on a tree that withholds it (the public mirror).
fn row_steps() -> Option<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/row-steps");
    std::fs::read_to_string(path).ok()
}

/// Each step of `text` with its undo, read as `vikectl` reads them.
fn steps_of(text: &str) -> Vec<(String, Option<String>)> {
    let mut steps: Vec<(String, Option<String>)> = Vec::new();
    for line in text.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.split_first() {
            None => {}
            Some((first, _)) if first.starts_with('#') => {}
            Some((&"undo", rest)) => {
                let step = steps.last_mut().expect("an undo has a step above it");
                assert!(step.1.is_none(), "a second undo under `{}`", step.0);
                step.1 = Some(rest.join(" "));
            }
            Some(_) => steps.push((words.join(" "), None)),
        }
    }
    steps
}

/// Why `line` is no command line its build runs, or `None` when it is one: this build's tree for a step; for an
/// undo, that tree or this build's table of the spellings it retired.
fn refusal(tree: &clap::Command, line: &str, undo: bool) -> Option<String> {
    // `vikectl` quotes and expands nothing, so a quote or an escape would reach the verb as itself: a value cannot
    // hold a blank.
    if line.contains(['"', '\'', '\\', '$', '`']) {
        return Some(format!("a quote or an escape in `{line}`"));
    }
    let why = check_examples(tree, &[format!("vike-cli {line}").as_str()]).err()?;
    let words: Vec<String> = line.split_whitespace().map(str::to_string).collect();
    let retired = words.split_first().is_some_and(|(plane, tail)| {
        plane == "config" && crate::cmd::config::retired_spelling(tail).is_some()
    });
    (!(undo && retired)).then_some(why)
}

#[test]
fn every_row_step_parses_and_every_undo_is_the_older_builds() {
    let Some(text) = row_steps() else {
        eprintln!("SKIPPED: deploy/row-steps is absent — the public mirror withholds it");
        return;
    };
    let tree = judging_tree();
    for (step, undo) in steps_of(&text) {
        if let Some(why) = refusal(&tree, &step, false) {
            panic!("a row step does not parse: {why}");
        }
        if let Some(why) = undo.as_deref().and_then(|undo| refusal(&tree, undo, true)) {
            panic!("an undo is no verb of the older build: {why}");
        }
    }
}

/// The rule, on planted lines: a step must parse here; an undo may spell a retired word, never an unknown one.
#[test]
fn an_undo_may_spell_a_retired_word_and_a_step_may_not() {
    let tree = judging_tree();
    assert_eq!(refusal(&tree, "config profile deactivate run", false), None);
    assert_eq!(
        refusal(&tree, "config activate run run-live", true),
        None,
        "the older build's word"
    );
    assert_eq!(
        refusal(&tree, "config rename-symbol --account 4 --from A --to B", true),
        None,
        "the older build's flag"
    );
    assert!(
        refusal(&tree, "config activate run run-live", false).is_some(),
        "a step is this build's"
    );
    assert!(refusal(&tree, "config no-such-verb", true).is_some(), "no build had it");
    assert!(refusal(&tree, "config set a \"b c\"", false).is_some(), "a quote");
}

/// The reader, on planted text: comments and blank lines skipped, an undo filed under the step above it.
#[test]
fn the_reader_files_each_undo_under_its_step() {
    let planted = "# a comment\n\nconfig set a 1\nundo config unset a\n  # indented comment\nconfig set b 2\n";
    assert_eq!(
        steps_of(planted),
        [
            ("config set a 1".to_string(), Some("config unset a".to_string())),
            ("config set b 2".to_string(), None)
        ]
    );
}
