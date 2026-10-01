//! A message literal may not break across lines without a continuation.
//!
//! # The defect this exists for, and how it shipped
//!
//! A Rust string literal that wraps onto the next source line WITHOUT a trailing `\` keeps the
//! newline **and the next line's indentation**. The author sees tidy source; the operator reads
//!
//! ```text
//! `data list` moved: it is `data hist ls` now. Every `data` verb lives in a GROUP
//!                       (hist | realtime | catalog | source) — the plane got too wide to be flat.
//! ```
//!
//! That is not hypothetical and it is not old: it was **measured in v0.1.32 on the deployed
//! binary**, by running the shipped `vike-cli` against the live datahub and grepping its own
//! refusals for a run of spaces. Two literals were affected, reaching five different refusals, and
//! a sweep of the tree then found **thirteen**. Every one of them had passed `cargo fmt`, `clippy
//! -D warnings`, the whole `vike-cli` suite and a full `verify-branch` — because **nothing in this
//! repository asserts what a message looks like**, only that it contains a needle.
//!
//! ⚠ **rustfmt cannot see it and never will.** It does not reformat string CONTENTS, so a literal
//! that is wrong inside is formatted correctly outside. That is precisely why this is a test and
//! not a lint.
//!
//! # What is allowed, and why each exemption is narrow
//!
//! * A **help page** is a different kind of literal: its newlines are the layout and its
//!   indentation is the column grid. A `const …USAGE…: &str` may therefore contain real newlines.
//!   ⚠ The exemption keys on the CONSTANT'S NAME rather than on shape, so a help page not called
//!   `USAGE` is not exempt and a refusal wrongly called `USAGE` is. That trade is deliberate: a
//!   name is what an author controls and can see, while guessing from content whether a newline
//!   was meant is exactly the judgement this gate exists to remove.
//! * A **raw string** (`r"…"`, `r#"…"#`) cannot carry a `\` continuation at all — the language does
//!   not offer one — so holding it to this rule would be demanding the impossible. Skipped.

use std::path::{Path, PathBuf};

/// The DATA PLANE's sources — `src/cmd/data.rs` and everything under `src/cmd/data/` — walked at
/// run time.
///
/// ⚠ **The scan is SCOPED, and what it is scoped away from is written down rather than implied.**
/// Pointed at this crate's whole `src/`, the scanner below reports **45**; adding `tests/` brings it
/// to **59**. They sit in three files and every one of them was read: `src/cmd/mcp.rs` (45 — the
/// two-call-gate sheet and the three `render_prompt` arms), `tests/exit_codes.rs` (2 — an inline
/// TOML run profile) and `tests/planted_binary_retry.rs` (12 — Rust source planted as a fixture for
/// its own gate). **NONE of them is this defect.** A prompt's blank line between numbered steps, a
/// TOML file's line breaks and a planted function's body are CONTENT, and in the one place a
/// continuation line is INDENTED — `render_prompt`'s nested bullets under step 2 of the triage
/// sheet — the three spaces are the layout the agent is meant to read. Separating content from the
/// defect needs the per-literal judgement this gate exists to avoid making, so widening is real
/// follow-on work with a real decision in it; what that work is, though, is a MARKING CONVENTION
/// rather than a sweep, because there is nothing there to fix.
///
/// ⚠ **This paragraph said 84 and named `src/cmd/init/content.rs` as a contributor. Neither
/// reproduces at this SHA**, and the second is the instructive half: that file contributes ZERO,
/// because its scaffold templates are RAW strings, which this scanner exempts by design — the
/// language offers them no continuation to demand. The counts above were measured by running this
/// file's own `findings_in` over both trees.
///
/// ⚠ **Neither existing exemption can be widened to cover `src/cmd/mcp.rs` honestly.** Those
/// literals are `format!` bodies inside `two_call_gate` and `render_prompt`, not consts, so the
/// `…USAGE…` name cannot apply to them at all; and while the first DOES open `"\`, that marker is
/// only honoured on a `const` — dropping that requirement is exactly what
/// `the_scan_reaches_the_files_it_claims_to`'s "a let is never a help page" case forbids, and it
/// would re-admit the defect wearing a `let`. A widening that also lets the defect through is worse
/// than no widening.
///
/// What IS held: the plane where the defect was measured on the deployed binary, at zero.
///
/// ⚠ Deliberately NOT a fixed file list. A list goes stale the moment a module is added, and a gate
/// that silently stops covering new code is worse than none — it reports a green for a file it
/// never opened. The cost is that this is path-keyed: if `src/cmd/data/` is renamed,
/// `the_scan_reaches_the_files_it_claims_to` fails loudly rather than passing over an empty set.
fn source_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let cmd = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("cmd");
    let mut out = vec![cmd.join("data.rs")];
    walk(&cmd.join("data"), &mut out);
    out.retain(|p| p.exists());
    out.sort();
    out
}

/// One offending line: the file, the 1-based line, and the source text.
#[derive(PartialEq, Eq)]
struct Finding {
    file: String,
    line: usize,
    text: String,
}

/// Scan one source text, CHARACTER BY CHARACTER, and report every ordinary string literal that
/// continues onto the next line without a `\`.
///
/// ⚠ **A line-oriented scan is not good enough here and the first attempt proved it**: counting
/// quotes per line reported dozens of false positives the moment it met an `r#"…"#` block, because
/// a raw string's own quotes are not delimiters in the sense the counter assumed. Rust's literal
/// grammar has four states that all end a "string" differently, so the scanner tracks all four.
fn findings_in_text(text: &str, name: &str) -> Vec<Finding> {
    #[derive(PartialEq)]
    enum S {
        Code,
        Line,
        Block,
        Str,
        Raw(usize),
    }

    let ch: Vec<char> = text.chars().collect();
    let mut state = S::Code;
    let mut out = Vec::new();
    let mut line = 1usize;
    let mut line_start = 0usize;
    let mut lit_is_usage = false;
    let mut i = 0usize;

    // the source line a given index sits on, for the message
    let line_text = |from: usize| -> String {
        let end = ch[from..].iter().position(|c| *c == '\n').map_or(ch.len(), |p| from + p);
        ch[from..end].iter().collect::<String>().trim_end().to_string()
    };

    while i < ch.len() {
        let c = ch[i];
        match state {
            S::Code => {
                if c == '\n' {
                    line += 1;
                    i += 1;
                    line_start = i;
                    continue;
                }
                if c == '/' && i + 1 < ch.len() && ch[i + 1] == '/' {
                    state = S::Line;
                    i += 2;
                    continue;
                }
                if c == '/' && i + 1 < ch.len() && ch[i + 1] == '*' {
                    state = S::Block;
                    i += 2;
                    continue;
                }
                if c == 'r' {
                    let mut j = i + 1;
                    let mut hashes = 0usize;
                    while j < ch.len() && ch[j] == '#' {
                        hashes += 1;
                        j += 1;
                    }
                    if j < ch.len() && ch[j] == '"' {
                        state = S::Raw(hashes);
                        i = j + 1;
                        continue;
                    }
                }
                if c == '\'' {
                    // a char literal or a lifetime; neither can open a string
                    i += if i + 2 < ch.len() && ch[i + 1] == '\\' { 4 } else { 2 };
                    continue;
                }
                if c == '"' {
                    let head: String = ch[line_start..i].iter().collect();
                    // A HELP PAGE declares itself in one of two ways, and both are marks the author
                    // made on purpose rather than shapes this gate infers:
                    //   * the constant is named …USAGE…, or
                    //   * the literal opens `"\` — a continuation on the very first character,
                    //     which is this crate's idiom for "the layout starts on the next line"
                    //     (`data.rs`'s own USAGE opens that way, and so does `catalog.rs`'s
                    //     PREAMBLE, which is a help page that is simply not called USAGE).
                    let opens_a_block = i + 2 < ch.len() && ch[i + 1] == '\\' && ch[i + 2] == '\n';
                    lit_is_usage =
                        head.contains("const ") && (head.contains("USAGE") || opens_a_block);
                    state = S::Str;
                    i += 1;
                    continue;
                }
                i += 1;
            }
            S::Line => {
                if c == '\n' {
                    state = S::Code;
                    line += 1;
                    i += 1;
                    line_start = i;
                    continue;
                }
                i += 1;
            }
            S::Block => {
                if c == '\n' {
                    line += 1;
                    line_start = i + 1;
                }
                if c == '*' && i + 1 < ch.len() && ch[i + 1] == '/' {
                    state = S::Code;
                    i += 2;
                    continue;
                }
                i += 1;
            }
            S::Raw(h) => {
                if c == '\n' {
                    line += 1;
                    line_start = i + 1;
                }
                if c == '"' {
                    let closes = (1..=h).all(|k| i + k < ch.len() && ch[i + k] == '#');
                    if closes {
                        state = S::Code;
                        i += h + 1;
                        continue;
                    }
                }
                i += 1;
            }
            S::Str => {
                if c == '\\' {
                    // a continuation is `\` immediately before the newline
                    if i + 1 < ch.len() && ch[i + 1] == '\n' {
                        line += 1;
                        i += 2;
                        line_start = i;
                        continue;
                    }
                    i += 2;
                    continue;
                }
                if c == '\n' {
                    if !lit_is_usage {
                        out.push(Finding {
                            file: name.to_string(),
                            line,
                            text: line_text(line_start),
                        });
                    }
                    line += 1;
                    i += 1;
                    line_start = i;
                    continue;
                }
                if c == '"' {
                    state = S::Code;
                    lit_is_usage = false;
                    i += 1;
                    continue;
                }
                i += 1;
            }
        }
    }
    out
}

fn findings_in(path: &Path) -> Vec<Finding> {
    let text = std::fs::read_to_string(path).expect("a source file this crate owns");
    let name = path
        .strip_prefix(env!("CARGO_MANIFEST_DIR"))
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    findings_in_text(&text, &name)
}

/// **THE GATE.** No message literal wraps without a continuation.
#[test]
fn no_message_literal_swallows_its_own_indentation() {
    let mut all = Vec::new();
    for f in source_files() {
        all.extend(findings_in(&f));
    }
    assert!(
        all.is_empty(),
        "{} message literal(s) break across lines with no `\\` continuation, so each carries a \
         newline and the next line's indentation into what an operator reads. Add the backslash.\n{}",
        all.len(),
        all.iter()
            .map(|f| format!("  {}:{}\n      {}", f.file, f.line, f.text))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The anti-vacuity control, because an empty result is this gate's PASSING state and an empty
/// result is also what a broken scan produces.
///
/// ⚠ Without this the gate reads green with the walk pointed at a directory that does not exist, or
/// with a scanner that never enters a literal — the exact failure mode the gate itself is written
/// against, one level up. So the detector is made to FIRE on a planted defect, and made to stay
/// silent on the three shapes that are legitimate.
#[test]
fn the_scan_reaches_the_files_it_claims_to() {
    let files = source_files();
    assert!(files.len() >= 5, "the walk found only {} data-plane files", files.len());
    for owed in ["cmd/data.rs", "data/catalog.rs", "data/source.rs", "data/gate.rs"] {
        assert!(
            files.iter().any(|f| f.to_string_lossy().replace('\\', "/").ends_with(owed)),
            "the walk must reach {owed} — it is in scope and was not opened: {files:?}"
        );
    }

    // it FIRES on the defect...
    let planted =
        "fn f() -> &'static str {\n    \"a sentence that wraps\n     onto the next line\"\n}\n";
    assert_eq!(findings_in_text(planted, "probe.rs").len(), 1, "the detector must fire");

    // ...and stays silent on a proper continuation,
    let ok =
        "fn f() -> &'static str {\n    \"a sentence that wraps \\\n     onto the next line\"\n}\n";
    assert!(findings_in_text(ok, "probe.rs").is_empty(), "a `\\` continuation is correct");

    // ...on a raw string, which cannot carry one at all,
    let raw = "fn f() -> &'static str {\n    r#\"a raw\n string\"#\n}\n";
    assert!(findings_in_text(raw, "probe.rs").is_empty(), "a raw string is exempt");

    // ...and on a help page, whose newlines ARE its layout.
    let usage = "const USAGE: &str = \"usage: thing\n  --flag  what it does\n\";\n";
    assert!(findings_in_text(usage, "probe.rs").is_empty(), "a USAGE const is exempt");

    // ...and on a help page that opens `"\` without being CALLED usage, which is the second
    // deliberate marker.
    let block = "const PREAMBLE: &str = \"\\\nusage: thing\n  --flag  what it does\";\n";
    assert!(findings_in_text(block, "probe.rs").is_empty(), "a `\"\\` block opener is exempt");

    // ...but a NON-usage const that neither names itself nor opens a block is NOT exempt, which is
    // what keeps the exemption narrow rather than a hole anything can be poured through.
    let notusage = "const NOTE: &str = \"a note\n  that wrapped\";\n";
    assert_eq!(findings_in_text(notusage, "probe.rs").len(), 1, "neither marker: not exempt");

    // ...and neither is a bare LET, whatever it opens with — the exemption requires a `const`.
    let letbound = "fn f() { let s = \"\\\nnot a help page\n  wrapped\"; }\n";
    assert_eq!(findings_in_text(letbound, "probe.rs").len(), 1, "a let is never a help page");
}

impl std::fmt::Debug for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.file, self.line)
    }
}
