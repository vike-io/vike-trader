//! Every `vike-cli …` line this repo tells someone to type, parsed through the real `clap` tree by
//! `vike_utility::cli::check_examples` (decision 0155). NOTHING runs. A renamed flag or verb fails here
//! instead of failing a reader.
//!
//! Read: `docs/*.md`, `docs/ops/*.md`, every crate's `CLAUDE.md`, `skills/*/SKILL.md` and the long text
//! of every `--help` page. `docs/decisions/` and `docs/superpowers/` are history and stay out. `docs/`
//! and the crate pages are not on the public mirror, so they are read at run time and skipped where
//! they are absent. A page's own edit selects this crate through
//! `xtask/src/ci/tables/readers.rs`'s `is_command_page`.
//!
//! Two strengths, because a page does two things with a command:
//!
//! * a line of a code FENCE is a line to type, so anything `clap` refuses is a failure;
//! * an inline `` `vike-cli secrets set` `` span or a line of help NAMES a command, so a missing value
//!   is not a failure, and an unknown word or flag still is.
//!
//! Placeholders are read as values (`<id>`, `@<price>`), `[--yes]` as `--yes`, and a word holding `…`
//! ends the line.
//!
//! shortcut: a line reached through a wrapper (`docker run --entrypoint vike-cli IMAGE …`,
//! `sudo -u vike vike-cli …`) is not judged, because its first word is not the program; handle it
//! when one goes stale unseen.

use std::path::Path;

use vike_utility::cli::check_examples;

use super::commands::whole_command;

/// A line of a page that names the program and is not a command line to type, each with why.
const NOT_A_COMMAND_LINE: &[(&str, &str)] = &[
    (
        "vike-cli strategy-status",
        "a retired verb named as retired: kill-switches.md's history of `status`, and a dated \
         rehearsal record",
    ),
    (
        "vike-cli backtest --profile",
        "past tense: datahub-the CI box.md's account of what a procedure ran on 2026-09-26",
    ),
    ("vike-cli data ls", "past tense: datahub-the CI box.md quoting what a bullet of its used to say"),
    (
        "vike-cli the CLI. `vike-cli config check` is the template unit's",
        "a directory listing in tradehub-the CI box.md, describing `bin/vike-cli`",
    ),
];

/// One line to judge, and where it came from.
struct Example {
    whence: String,
    line: String,
    /// A line to type: anything `clap` refuses fails it.
    strict: bool,
}

/// The tree the lines are judged by. `trade` and `data` take any other word as an external subcommand
/// so that they can refuse it BY NAME after the parse, so a stale word would parse here: this tree
/// refuses it. Built once: `check_examples` clones it per line.
fn judging_tree() -> clap::Command {
    fn no_external(cmd: clap::Command) -> clap::Command {
        cmd.external_subcommand_value_parser(clap::builder::Resettable::Reset)
            .mut_subcommands(no_external)
    }
    let mut tree = no_external(whole_command());
    tree.build();
    tree
}

/// Each command line in `text`: from a `vike-cli` word (bare, or a path ending in it) in the place a
/// shell runs a program. Everything else naming the word is not one: a cargo line (`-p vike-cli`), an
/// `scp` or a `gh --pattern` of the file, prose in a fence, the program's own output (`vike-cli 0.1.49
/// (git …`, or `vike-cli backend: settings database …`), and a verb that is itself a placeholder.
fn commands_in(text: &str) -> Vec<String> {
    const WORD: &str = "vike-cli";
    let mut out = Vec::new();
    for (at, _) in text.match_indices(WORD) {
        let tail = &text[at + WORD.len()..];
        // `"$R/bin/vike-cli" secrets init`: the quote closes the path.
        let tail = tail.strip_prefix('"').unwrap_or(tail);
        if !tail.starts_with(' ') {
            continue;
        }
        let head = &text[..at];
        let word_start = head.rfind(char::is_whitespace).map_or(0, |space| space + 1);
        let path_prefix = &head[word_start..];
        let runs_a_program = match head[..word_start].split_whitespace().next_back() {
            None => true,
            Some(previous) => {
                previous.contains('=')
                    || ["|", "&&", "||", ";", "$", "sudo", "exec", "time", "env", "then", "do"]
                        .contains(&previous)
            }
        };
        let a_path = path_prefix.is_empty()
            || (path_prefix.ends_with('/')
                && !path_prefix.starts_with("target/")
                && !path_prefix.starts_with('-'));
        if !runs_a_program || !a_path {
            continue;
        }
        let line = normalized(tail);
        let mut words = line.split_whitespace();
        let first = words.next().unwrap_or_default();
        let a_verb = first.starts_with(|c: char| c.is_ascii_lowercase() || c == '-');
        let output = first.ends_with(':') || words.take(3).any(|word| word.ends_with(':'));
        if a_verb && !output {
            out.push(format!("{WORD} {line}"));
        }
    }
    out
}

/// A placeholder read as the value `1` (some parsers take only a number, none here refuses one),
/// brackets around an option dropped, and the line cut at a word holding an ellipsis.
fn normalized(tail: &str) -> String {
    let mut out = String::new();
    let mut rest = tail.trim();
    while let Some(open) = rest.find('<') {
        let close = rest[open..].find('>').map(|at| open + at);
        let placeholder = close.is_some_and(|_| {
            rest[open + 1..].chars().next().is_some_and(|c| !c.is_whitespace() && c != '=')
        });
        let Some(close) = close.filter(|_| placeholder) else { break };
        out.push_str(&rest[..open]);
        out.push('1');
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    let mut kept: Vec<&str> = Vec::new();
    for word in out.split_whitespace() {
        if word.contains('…') || word.contains("...") {
            break;
        }
        kept.push(word);
    }
    kept.join(" ").replace(" [", " ").replace("] ", " ").trim_end_matches(']').to_string()
}

/// The lines of one markdown page: every line of a code fence, every inline code span outside one.
fn examples_in_markdown(whence: &str, page: &str) -> Vec<Example> {
    let mut out = Vec::new();
    let mut fenced = false;
    let mut joined = String::new();
    for (number, raw) in page.lines().enumerate() {
        let number = number + 1;
        if raw.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            let line = raw.trim();
            if line.starts_with('#') {
                continue;
            }
            if let Some(continued) = line.strip_suffix('\\') {
                joined.push_str(continued);
                joined.push(' ');
                continue;
            }
            joined.push_str(line);
            let whole = std::mem::take(&mut joined);
            for line in commands_in(&whole) {
                let whence = format!("{whence}:{number}");
                out.push(Example { whence, line, strict: true });
            }
            continue;
        }
        for span in raw.split('`').skip(1).step_by(2) {
            for line in commands_in(span) {
                let whence = format!("{whence}:{number}");
                out.push(Example { whence, line, strict: false });
            }
        }
    }
    out
}

/// The lines of every `--help` page: a span in backticks, or a line that starts with the program.
fn examples_in_help(command: &clap::Command, path: &str, out: &mut Vec<Example>) {
    let pages = [
        command.get_long_about(),
        command.get_before_help(),
        command.get_before_long_help(),
        command.get_after_help(),
        command.get_after_long_help(),
    ];
    let args = command.get_arguments().flat_map(|arg| [arg.get_help(), arg.get_long_help()]);
    for text in pages.into_iter().chain(args).flatten() {
        let text = text.to_string();
        for raw in text.lines() {
            let spans = raw.split('`').skip(1).step_by(2).flat_map(commands_in);
            let bare = commands_in(raw)
                .into_iter()
                .take(raw.trim_start().starts_with("vike-cli") as usize);
            for line in spans.chain(bare) {
                out.push(Example { whence: format!("`{path} --help`"), line, strict: false });
            }
        }
    }
    for sub in command.get_subcommands() {
        examples_in_help(sub, &format!("{path} {}", sub.get_name()), out);
    }
}

/// What the tree says about one line, when it refuses it for a reason this line's strength counts.
fn refusal(tree: &clap::Command, example: &Example) -> Option<String> {
    let reason = check_examples(tree, &[example.line.as_str()]).err()?;
    let counts = example.strict
        || reason.contains("unrecognized subcommand")
        || reason.contains("unexpected argument");
    // `vike-cli config <verb> --help` names no verb, so a placeholder there says nothing stale.
    let a_placeholder_verb = reason.contains("unrecognized subcommand '1'");
    let excused = NOT_A_COMMAND_LINE.iter().any(|(line, _)| example.line == *line);
    (counts && !a_placeholder_verb && !excused).then(|| format!("{}: {reason}", example.whence))
}

fn markdown_files(dir: &Path, nested: Option<&str>) -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        eprintln!("SKIPPED: {} is absent — the public mirror withholds it", dir.display());
        return Vec::new();
    };
    let mut files: Vec<std::path::PathBuf> = entries
        .map(|entry| entry.expect("a readable dir entry").path())
        .filter_map(|path| match nested {
            Some(name) => Some(path.join(name)).filter(|file| file.is_file()),
            None => Some(path).filter(|file| file.extension().is_some_and(|e| e == "md")),
        })
        .collect();
    files.sort();
    files
}

#[test]
fn every_command_line_on_a_page_parses() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let tree = judging_tree();
    let mut pages = markdown_files(&root.join("../../docs"), None);
    pages.extend(markdown_files(&root.join("../../docs/ops"), None));
    pages.extend(markdown_files(&root.join("../../skills"), Some("SKILL.md")));
    pages.extend(markdown_files(&root.join("../../crates"), Some("CLAUDE.md")));
    pages.extend(markdown_files(&root.join("../../crates/bridges"), Some("CLAUDE.md")));
    let mut seen = 0;
    let mut refused = Vec::new();
    for page in pages {
        let text = std::fs::read_to_string(&page).expect("a readable page");
        let whence = page.display().to_string();
        let whence = whence.rsplit_once("/../../").map_or(whence.as_str(), |(_, rel)| rel);
        for example in examples_in_markdown(whence, &text) {
            seen += 1;
            refused.extend(refusal(&tree, &example));
        }
    }
    assert!(seen > 100 || !root.join("../../docs/ops").is_dir(), "read only {seen} command lines");
    assert!(
        refused.is_empty(),
        "{} documented command lines do not parse:\n  {}",
        refused.len(),
        refused.join("\n  ")
    );
}

#[test]
fn every_command_line_in_a_help_page_parses() {
    let tree = judging_tree();
    let mut examples = Vec::new();
    examples_in_help(&tree, "vike-cli", &mut examples);
    assert!(!examples.is_empty(), "the walk found no command line in any help page");
    let refused: Vec<String> = examples.iter().filter_map(|e| refusal(&tree, e)).collect();
    assert!(
        refused.is_empty(),
        "{} command lines in --help do not parse:\n  {}",
        refused.len(),
        refused.join("\n  ")
    );
}

/// What the extractor reads, on planted text.
#[test]
fn the_extractor_reads_placeholders_and_options_and_leaves_the_package_alone() {
    let found = |text: &str| commands_in(text);
    assert_eq!(
        found("vike-cli trade recon confirm <id> --node <a b> [--yes]"),
        ["vike-cli trade recon confirm 1 --node 1 --yes"]
    );
    assert_eq!(found("$P/bin/vike-cli config check"), ["vike-cli config check"]);
    assert_eq!(found("$k | vike-cli secrets set KEY"), ["vike-cli secrets set KEY"]);
    assert_eq!(found("A=1 \"$R/bin/vike-cli\" secrets init"), ["vike-cli secrets init"]);
    assert_eq!(found("vike-cli config set venue.… "), ["vike-cli config set"]);
    assert!(found("cargo build -p vike-cli --bin vike-cli").is_empty());
    assert!(found("install -m755 target/release/vike-cli \"$R/bin/\"").is_empty());
    assert!(found("scp vike-cli the CI box:/x").is_empty());
    assert!(found("add -f build_cli=true to build vike-cli too").is_empty());
    assert!(found("vike-cli-mcp --x").is_empty());
    assert!(found("vike-cli 0.1.49 (git 542a clean,").is_empty(), "--version's output");
    assert!(found("vike-cli backend: settings database").is_empty(), "the program's own error");
    assert!(found("vike-cli <verb> --help").is_empty(), "a verb that is a placeholder");

    let tree = judging_tree();
    let judge = |line: &str, strict: bool| {
        let example = Example { whence: "t".to_string(), line: line.to_string(), strict };
        refusal(&tree, &example)
    };
    assert_eq!(judge("vike-cli config check", true), None);
    assert!(judge("vike-cli config no-such-verb", false).is_some(), "an unknown verb counts");
    assert!(
        judge("vike-cli config check --no-such-flag", false).is_some(),
        "an unknown flag counts"
    );
    assert!(judge("vike-cli trade no-such-word", false).is_some(), "an external word counts here");
    assert_eq!(judge("vike-cli secrets", false), None, "a span may name a plane");
    assert!(judge("vike-cli secrets", true).is_some(), "a line to type may not stop at a plane");
}

/// Every `vike-cli` command a prompt of the MCP server hands a human parses through the real tree. An
/// agent relays these to an operator, who types them. The two programs are siblings, so this reads the
/// text from its golden (`crates/vike-mcp/tests/shipped_mcp/mcp_wire_snapshot.rs` holds that file equal
/// to what the server serves).
#[test]
fn every_command_line_in_the_mcp_prompts_parses() {
    fn spans_in<'a>(value: &'a serde_json::Value, out: &mut Vec<&'a str>) {
        match value {
            serde_json::Value::String(text) => out.extend(text.split('`').skip(1).step_by(2)),
            serde_json::Value::Array(items) => items.iter().for_each(|item| spans_in(item, out)),
            serde_json::Value::Object(fields) => fields.values().for_each(|v| spans_in(v, out)),
            _ => {}
        }
    }
    let golden = include_str!("../../../vike-mcp/tests/fixtures/mcp_wire/prompts.full.json");
    let golden: serde_json::Value = serde_json::from_str(golden).expect("the golden is JSON");
    let mut spans = Vec::new();
    spans_in(&golden, &mut spans);
    let tree = judging_tree();
    let examples: Vec<Example> = spans
        .into_iter()
        .flat_map(commands_in)
        .map(|line| Example { whence: "an MCP prompt".to_string(), line, strict: false })
        .collect();
    // Non-vacuity: the sheet that hands the human these commands is still being read.
    let named = |verb: &str| examples.iter().any(|e| e.line.starts_with(verb));
    assert!(named("vike-cli secrets account set-tier"), "the prompts stopped reading");
    let refused: Vec<String> = examples.iter().filter_map(|e| refusal(&tree, e)).collect();
    assert!(
        refused.is_empty(),
        "{} command lines in the MCP prompts do not parse:\n  {}",
        refused.len(),
        refused.join("\n  ")
    );
}

/// The MCP server's `instructions` name only real `vike-cli` commands. The rot the whole repository
/// is organised against: prose that names a command the binary does not have sends an operator to a
/// terminal to type something that fails, on the word of the surface itself.
///
/// The two programs are siblings, so this reads the server's text from its goldens
/// (`crates/vike-mcp/tests/shipped_mcp/mcp_wire_snapshot.rs` holds those files equal to what it
/// serves). Each verb is checked against [`crate::COMMANDS`], and each SUBCOMMAND and FLAG against the
/// owning module's own help page, rendered from its `clap` command. Neither is a copy.
///
/// ⚠ Two bounds, declared rather than implied. Arguments are NOT checked (`run.toml`,
/// `binance:BTCUSDT:1h` and `180` are values an operator supplies, not names this workspace owns),
/// and a NON-`vike-cli` binary is not checked here at all: this crate cannot see another crate's
/// manifest, so `crates/vike-ops/tests/docs/mcp_instructions_gate.rs` holds `vike-backend` to a real
/// `[[bin]]` from outside.
#[test]
fn the_mcp_instructions_name_only_real_commands() {
    fn usage_for(verb: &str) -> Option<String> {
        match verb {
            "data" => Some(crate::cmd::args::help_tree(&crate::cmd::data::command())),
            "secrets" => Some(crate::cmd::secrets::usage()),
            "backtest" => Some(crate::cmd::args::help_tree(&crate::cmd::backtest::command())),
            _ => None,
        }
    }

    let verbs: Vec<&str> = crate::COMMANDS.iter().map(|(name, _)| *name).collect();
    let mut checked = 0usize;
    for text in [
        include_str!("../../../vike-mcp/tests/fixtures/mcp_wire/instructions.full.txt"),
        include_str!("../../../vike-mcp/tests/fixtures/mcp_wire/instructions.offline.txt"),
    ] {
        // Every command the instructions name is inside backticks, so the spans ARE the commands.
        for command in text.split('`').skip(1).step_by(2) {
            let mut words = command.split_whitespace();
            if words.next() != Some("vike-cli") {
                continue;
            }
            // A span that is JUST the binary name, naming the product rather than invoking it,
            // carries no verb to check.
            let Some(verb) = words.next() else { continue };
            assert!(
                verbs.contains(&verb),
                "the MCP instructions name `vike-cli {verb}`, which is not a registered \
                 subcommand. The dispatcher's roster is {verbs:?}."
            );
            let usage = usage_for(verb).unwrap_or_else(|| {
                panic!(
                    "the MCP instructions name `vike-cli {verb}` and this test has no `USAGE` to \
                     hold its subcommands and flags to — add the arm, do not delete the check"
                )
            });
            for word in words {
                // Values are the operator's, not ours (see the bound above).
                if !word.starts_with("--") && (word.contains(['.', ':']) || word.starts_with('$')) {
                    continue;
                }
                if word.chars().all(|c| c.is_ascii_digit()) {
                    continue;
                }
                assert!(
                    usage.contains(word),
                    "the MCP instructions name `{word}` under `vike-cli {verb}`, which that \
                     command's own USAGE does not offer"
                );
                checked += 1;
            }
        }
    }
    // Non-vacuity: a text whose backtick spans stopped parsing would pass every assertion above by
    // checking nothing at all.
    assert!(checked >= 4, "only {checked} subcommand/flag(s) were checked — the parse broke");
}
