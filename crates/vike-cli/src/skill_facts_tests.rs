//! `scripts/gen_skills.sh` renders `skills/` from this crate's exit ladder, its verb table and the
//! policy bounds, and reads them from a committed file so that rendering costs no build:
//! `crates/vike-cli/tests/fixtures/skill_facts.tsv`. This is that file's drift gate. To regenerate it,
//! in a lane (nothing compiles on the dev box), then read the diff and run `scripts/gen_skills.sh`:
//!
//! ```sh
//! cargo test -p vike-cli --lib -- --ignored bless_skill_facts
//! ```

use vike_config::policy;

use super::COMMANDS;
use crate::exit::Exit;

const COMMITTED: &str = include_str!("../tests/fixtures/skill_facts.tsv");

/// The named bounds a skill states. Named one by one because a constant has no list to walk: a name
/// here that stops existing does not compile, and a skill naming one absent here does not render.
const POLICY_BOUNDS: [(&str, u64); 8] = [
    ("RECOMMENDED_DEADMAN_TIMEOUT_MS", policy::RECOMMENDED_DEADMAN_TIMEOUT_MS),
    ("DEADMAN_DISABLED_MS", policy::DEADMAN_DISABLED_MS),
    ("MIN_DEADMAN_TIMEOUT_MS", policy::MIN_DEADMAN_TIMEOUT_MS),
    ("MAX_DEADMAN_TIMEOUT_MS", policy::MAX_DEADMAN_TIMEOUT_MS),
    ("DEFAULT_LINK_DEADMAN_GRACE_MS", policy::DEFAULT_LINK_DEADMAN_GRACE_MS),
    ("LINK_DEADMAN_DISABLED_MS", policy::LINK_DEADMAN_DISABLED_MS),
    ("MIN_LINK_DEADMAN_GRACE_MS", policy::MIN_LINK_DEADMAN_GRACE_MS),
    ("MAX_LINK_DEADMAN_GRACE_MS", policy::MAX_LINK_DEADMAN_GRACE_MS),
];

/// One `EXIT` row per rung of `exit.rs`'s `Exit`: its number (the rung's own value, from `Exit::ALL`,
/// since a discriminant may name a constant, `vike_utility::cli::OUTCOME_UNKNOWN_EXIT`, which the
/// source text cannot evaluate), and its doc's first sentence, which is where each rung says what a
/// caller may do about it. Read off the source because a doc comment reaches nothing at run time;
/// intra-doc links come out as plain code spans for a markdown reader.
fn exit_rows() -> Vec<String> {
    let source = include_str!("exit.rs");
    let (_, rest) = source.split_once("\npub enum Exit {\n").expect("exit.rs holds the enum");
    let (body, _) = rest.split_once("\n}\n").expect("the enum ends");
    let mut rows = Vec::new();
    let mut doc = String::new();
    for line in body.lines().map(str::trim_start) {
        if let Some(text) = line.strip_prefix("///") {
            if !doc.is_empty() {
                doc.push(' ');
            }
            doc.push_str(text.strip_prefix(' ').unwrap_or(text));
        } else if let Some((variant, _)) = line.strip_suffix(',').and_then(|l| l.split_once(" = "))
        {
            let rung = Exit::ALL
                .into_iter()
                .find(|r| format!("{r:?}") == variant)
                .unwrap_or_else(|| panic!("`Exit::{variant}` is not in `Exit::ALL`"));
            let sentence = doc.find(". ").map_or(doc.as_str(), |end| &doc[..=end]);
            let sentence = sentence.replace("[`", "`").replace("`]", "`");
            rows.push(format!("EXIT\t{}\t{variant}\t{}", rung as u8, sentence.trim()));
            doc.clear();
        } else {
            doc.clear();
        }
    }
    rows
}

fn skill_facts() -> String {
    let verbs = COMMANDS.iter().map(|(name, summary)| format!("VERB\t{name}\t{summary}"));
    let bounds = POLICY_BOUNDS.iter().map(|(name, value)| format!("CONST\t{name}\t{value}"));
    let mut rows = exit_rows();
    rows.extend(verbs);
    rows.extend(bounds);
    rows.join("\n") + "\n"
}

#[test]
fn the_committed_skill_facts_are_what_this_crate_says() {
    assert_eq!(
        COMMITTED,
        skill_facts(),
        "tests/fixtures/skill_facts.tsv is not what this crate says. In a lane: cargo test -p \
         vike-cli --lib -- --ignored bless_skill_facts, then scripts/gen_skills.sh, and read both diffs"
    );
}

/// The enum's numbers ARE the exit codes, so each row is checked against the value a process exits
/// on. A walk of the source that lost a rung would otherwise bless itself.
#[test]
fn every_exit_rung_is_read_with_its_own_number() {
    let rows = exit_rows();
    assert_eq!(rows.len(), Exit::ALL.len(), "{rows:?}");
    for (row, rung) in rows.iter().zip(Exit::ALL) {
        assert!(row.starts_with(&format!("EXIT\t{}\t{rung:?}\t", rung as u8)), "{row}");
    }
}

#[test]
#[ignore = "writes into the source tree: run it deliberately, then read the diff"]
fn bless_skill_facts() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/skill_facts.tsv");
    std::fs::write(path, skill_facts()).expect("write the skill facts");
}
