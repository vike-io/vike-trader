//! **The CLI half of the step-0 goldens**: the first line of each verb's `--help`, and what each verb
//! answers to a flag it does not have. `tests/fixtures/cli_surface.json` is the golden, and
//! `cargo test -p vike-cli --test shipped_cli -- --ignored bless_` in a lane rewrites it: read the
//! diff. `crates/vike-mcp/tests/shipped_mcp/mcp_wire_snapshot.rs` is the server half.
//!
//! The verb list is read off the shipped binary's own root `--help`, so a verb that appears or goes
//! changes this golden. The exit code of `--help` and the accepted command lines are
//! `crates/vike-cli/tests/shipped_cli/argv_accepted_ops.rs`'s, and are not repeated here.

use serde_json::{Value, json};

use crate::common::run;

const GOLDEN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cli_surface.json");

fn first_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).lines().next().unwrap_or_default().to_string()
}

/// The verbs under the root `--help`'s `Commands:` heading, in its order, without clap's own `help`.
fn verbs_on_the_root_help(help: &str) -> Vec<String> {
    help.lines()
        .skip_while(|line| !line.starts_with("Commands:"))
        .skip(1)
        .take_while(|line| line.starts_with(' '))
        .filter_map(|line| line.split_whitespace().next())
        .filter(|verb| *verb != "help")
        .map(str::to_string)
        .collect()
}

/// The surface as the shipped binary states it now.
fn surface() -> Value {
    let root = run(&["--help"], &[]);
    let verbs = verbs_on_the_root_help(&String::from_utf8_lossy(&root.stdout));
    assert!(verbs.len() > 5, "the root help's parse broke, it found {verbs:?}");
    let rows: Vec<Value> = verbs
        .iter()
        .map(|verb| {
            let help = run(&[verb.as_str(), "--help"], &[]);
            let refused = run(&[verb.as_str(), "--definitely-not-a-flag"], &[]);
            json!({
                "verb": verb,
                "help_first_line": first_line(&help.stdout),
                "bad_flag_exit": refused.status.code(),
                "bad_flag_first_line": first_line(&refused.stderr),
            })
        })
        .collect();
    json!({
        "what": "each verb's `--help` first line, and its answer to a flag it does not have",
        "root_help_first_line": first_line(&root.stdout),
        "verbs": rows,
    })
}

#[test]
fn the_cli_surface_matches_its_golden() {
    let committed = std::fs::read_to_string(GOLDEN)
        .unwrap_or_else(|e| panic!("the committed golden at {GOLDEN}: {e}"));
    let golden: Value = serde_json::from_str(&committed).expect("a committed golden is JSON");
    assert_eq!(
        golden,
        surface(),
        "the CLI surface moved. If this is intended: cargo test -p vike-cli --test shipped_cli -- \
         --ignored bless_  (in a lane), then read the diff."
    );
}

/// THE WRITER: the module doc has the command.
#[test]
#[ignore = "writes into the source tree — run it deliberately, then read the diff"]
fn bless_cli_surface_golden() {
    let pretty = serde_json::to_string_pretty(&surface()).expect("a golden is JSON");
    std::fs::write(GOLDEN, format!("{pretty}\n"))
        .unwrap_or_else(|e| panic!("could not write {GOLDEN}: {e}"));
}
