//! The shipped manifests: `server.json` and every skill name only tools this server serves.

use super::*;

#[test]
fn the_registry_manifest_lists_every_tool_this_server_serves() {
    // The manifest is what a registry user reads BEFORE installing. A tool added to
    // `tools_spec` and not to the manifest is a promise the listing does not make; one in the
    // manifest and not the server is a promise it cannot keep. Neither is visible to any other
    // gate — the manifest is a hand-written file at the repository root, and nothing else in
    // this workspace reads it.
    //
    // ⚠ It compares `_vike.tools`, a block WE own, rather than anything the upstream registry
    // schema names. The schema has already changed more than once; a gate keyed on an upstream
    // field would silently stop checking the day a key was renamed.
    const MANIFEST: &str = include_str!("../../../../../../server.json");
    let manifest: Value = serde_json::from_str(MANIFEST).expect("server.json is valid JSON");
    let listed: Vec<&str> = manifest["_vike"]["tools"]
        .as_array()
        .expect("server.json carries _vike.tools")
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect();
    let spec = tools_spec();
    let served: Vec<&str> =
        spec.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(listed, served, "server.json's tool list has drifted from tools_spec");
    assert_eq!(
        manifest["version"].as_str(),
        Some(SERVER_VERSION),
        "server.json's version must match the crate the server reports at initialize"
    );
    // The registry's `description` is capped at 100 characters (the 2025-12-11 schema), and a
    // manifest that overruns it is refused at publish time rather than here — which is the
    // wrong place to find out, since publishing is a deliberate one-off act.
    let description = manifest["description"].as_str().expect("server.json carries description");
    assert!(
        (1..=100).contains(&description.chars().count()),
        "the registry caps `description` at 100 characters; this one is {}",
        description.chars().count()
    );
    // The mirror's FORBID scan aborts the WHOLE publish on a box name or a private path, and
    // this file is the first root-level manifest to ship. Catch it here, where the failure
    // names the manifest, rather than at release time where it names a grep.
    assert!(
        !MANIFEST.contains("the latency box") && !MANIFEST.contains("the CI box"),
        "server.json must name no host — the mirror's FORBID scan aborts the publish"
    );
}

/// Every `skills/<name>/SKILL.md` teaches a procedure over THIS server's tools, and nothing but
/// the file itself says which tools those are — so this is the one place a tool change can
/// redden the skill that teaches it. The failure it guards is the M1 one: the docs described
/// the preview gate for a full day after the gate had changed and nobody noticed, because a
/// page that names a tool is never compiled against the tool. A skill is worse than a page
/// here — it is INSTALLED (`npx skills add` symlinks the file into an agent's skill
/// directory), so a stale step is not read by a human who might doubt it; it is executed by an
/// agent that will not.
///
/// What is pinned, per skill, against the Agent Skills spec and against `tools_spec`:
///   * `name` equals the directory (the spec's identity rule) and matches its pattern;
///   * `description` is 30–1024 chars — it is the TRIGGER (the spec has no separate field),
///     so an empty one is a skill that never fires and an overlong one is refused at install;
///   * every read field is a scalar a YAML parser would accept — no `: ` or ` #` inside an
///     unquoted value, no leading indicator — because this scan is not a parser and once
///     passed a description js-yaml refused (the file is the product, and it did not install);
///   * under 500 lines, the spec's ceiling;
///   * every `metadata.tools` entry is a tool this server serves — a renamed tool reddens here;
///   * every served tool the body names in backticks IS declared, so `metadata.tools` cannot
///     under-report what the skill drives (the convention this buys: backticks mean "a tool
///     this procedure calls", and a tool merely referred to is written bare);
///   * a declared WRITE tool comes with `preview_token` in the body — a skill that teaches a
///     write without the two-call gate teaches an agent to read a preview as a send;
///   * the text names no operator file the public mirror withholds (`CLAUDE.md`, `justfile`,
///     `scripts/`, `.github/`, the decision and plan trees) and pins no `.rs:NNN` line — both
///     rot silently, and neither can be followed from an install.
///
/// Read from disk at test time, deliberately not `include_str!`: the mirror ships `skills/`
/// beside `crates/`, and a compile-time embed would make every one of those markdown files a
/// build input of this crate.
///
/// ⚠ **The floor that keeps this from passing over a mis-resolved directory is DERIVED, and it
/// used to be the number ten in an `assert!` and in this doc.** That is the failure the whole
/// `skills/` tree is now generated to avoid one level down: a count written beside a set drifts
/// from it, and this one would have been wrong the moment a skill was added — while still
/// reading green, because it was a `>=`. So the property asserted instead is a POSITIVE one
/// that says the same thing without naming a number: every SUBDIRECTORY of `skills/` carries a
/// `SKILL.md`, and there is at least one. A mis-resolved path has no subdirectories and fails
/// on the second half; a directory that lost its page fails on the first.
#[test]
fn every_skill_names_only_tools_this_server_serves() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills");
    let served: Vec<String> = tools_spec()
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("skills directory {}: {e}", dir.display()))
        .flatten()
        .collect();
    entries.sort_by_key(|e| e.file_name());
    let mut seen = 0;
    for entry in entries {
        if !entry.path().is_dir() {
            // `skills/README.md` — the generated index — sits beside the directories.
            continue;
        }
        let path = entry.path().join("SKILL.md");
        let skill = entry.file_name().to_string_lossy().to_string();
        assert!(
            path.is_file(),
            "skills/{skill} is a directory with no SKILL.md — the Agent Skills spec identifies \
                 a skill by that file, so this one installs as nothing"
        );
        let text = std::fs::read_to_string(&path).unwrap();
        seen += 1;
        // Frontmatter is the block between the opening `---` and the next; the body follows.
        let rest = text.strip_prefix("---\n").unwrap_or_else(|| panic!("{skill}: no frontmatter"));
        let end =
            rest.find("\n---\n").unwrap_or_else(|| panic!("{skill}: unterminated frontmatter"));
        let (front, body) = (&rest[..end], &rest[end + 5..]);
        // A top-level or `metadata:`-nested `key: value` line, as written. No YAML
        // dependency: the two shapes the spec allows here are both one line.
        let raw_field = |key: &str| -> Option<&str> {
            front
                .lines()
                .find_map(|l| l.trim_start().strip_prefix(key)?.strip_prefix(':'))
                .map(str::trim)
        };
        // ...and the same value with its quotes stripped, for the content assertions below.
        let field = |key: &str| raw_field(key).map(|v| v.trim_matches('"'));
        // ⚠ A line scan is not a YAML parser, and the gap bit once: a description quoting the
        // error text `control command not sent: Gone` passed here — it is 30–1024 characters —
        // while js-yaml (what `npx skills add` and gray-matter wrap) refused the file with
        // `bad indentation of a mapping entry`, because `: ` inside an UNQUOTED scalar ends
        // the scalar. So every field the scan reads is held to the plain-scalar rules a YAML
        // parser would apply, unless the whole value is one quoted scalar: no `: ` or ` #`
        // inside it, no trailing `:`, and no leading indicator character. Read from disk by a
        // parser this crate does not carry, the failure was an uninstallable skill behind a
        // green gate; here it is a named substring.
        let plain_scalar_hazard = |raw: &str| -> Option<String> {
            let quoted = |q: char| raw.len() >= 2 && raw.starts_with(q) && raw.ends_with(q);
            if raw.is_empty() || quoted('"') || quoted('\'') {
                return None;
            }
            for needle in [": ", " #"] {
                if raw.contains(needle) {
                    return Some(format!("contains {needle:?}"));
                }
            }
            if raw.ends_with(':') {
                return Some("ends with ':'".to_string());
            }
            let first = raw.chars().next().unwrap_or(' ');
            if "-?:,[]{}#&*!|>'\"%@`".contains(first) {
                return Some(format!("starts with the indicator {first:?}"));
            }
            None
        };
        for key in ["name", "description", "tools", "source"] {
            if let Some(raw) = raw_field(key)
                && let Some(why) = plain_scalar_hazard(raw)
            {
                panic!(
                    "{skill}: frontmatter `{key}` is an unquoted scalar that {why} — a YAML \
                         parser ends the value there and refuses the file, so the skill cannot \
                         be installed. Reword it, or quote the whole value"
                );
            }
        }
        let name = field("name").unwrap_or_else(|| panic!("{skill}: no `name`"));
        assert_eq!(name, skill, "{skill}: `name` must equal the directory name");
        let pattern_ok = !name.is_empty()
            && name.len() <= 64
            && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !name.starts_with('-')
            && !name.ends_with('-')
            && !name.contains("--");
        assert!(pattern_ok, "{skill}: `name` {name:?} breaks the spec's pattern");
        let desc_chars = field("description").unwrap_or_default().chars().count();
        assert!(
            (30..=1024).contains(&desc_chars),
            "{skill}: description is {desc_chars} chars, must be 30–1024 — it is the trigger"
        );
        let lines = text.lines().count();
        assert!(lines < 500, "{skill}: {lines} lines; the spec's ceiling is 500");
        let declared: Vec<&str> = field("tools").unwrap_or_default().split_whitespace().collect();
        for t in &declared {
            assert!(
                served.iter().any(|s| s == *t),
                "{skill}: metadata.tools names `{t}`, which this server does not serve \
                     (tools_spec serves {served:?})"
            );
        }
        for s in &served {
            if body.contains(&format!("`{s}`")) {
                assert!(
                    declared.contains(&s.as_str()),
                    "{skill}: the body names `{s}` but metadata.tools does not declare it — \
                         the declaration under-reports what the skill drives"
                );
            }
        }
        for t in &declared {
            if WRITE_TOOLS.contains(t) {
                assert!(
                    body.contains("preview_token"),
                    "{skill}: teaches the write tool `{t}` without `preview_token` — without \
                         the two-call gate every call is a preview, and an agent taught to read \
                         one as a send has been taught a refusal"
                );
            }
        }
        for needle in
            ["CLAUDE.md", "justfile", "scripts/", ".github/", "docs/decisions", "docs/superpowers"]
        {
            assert!(!text.contains(needle), "{skill}: names {needle}, which the mirror withholds");
        }
        for (i, line) in text.lines().enumerate() {
            let mut tail = line;
            while let Some(at) = tail.find(".rs:") {
                tail = &tail[at + 4..];
                assert!(
                    !tail.starts_with(|c: char| c.is_ascii_digit()),
                    "{skill} line {}: {:?} pins a line number — cite by symbol",
                    i + 1,
                    line.trim()
                );
            }
        }
    }
    assert!(
        seen > 0,
        "no skills under {} — an empty answer here is a wrong path, not a smaller package",
        dir.display()
    );
}
