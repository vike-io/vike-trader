//! **Every composition root either writes a BOOT ANCHOR or declares why it does not.**
//!
//! # Why a gate rather than a convention
//!
//! `crates/vike-boot/src/lib.rs`'s `journal_boot_settings` is the whole of the anchor's behaviour
//! and `tests/boot_journal.rs` gates it end to end — against a real directory tree, with the
//! bracket property asserted in both directions. **None of that notices if no root ever calls it.**
//! That is not a hypothetical failure mode in this repository: `vike_model::change_journal`'s
//! `boot_settings` record type shipped BUILT AND TESTED and called by nothing, and its own module
//! doc had to carry a "wired? no" row to admit it.
//!
//! So this file asks the other question. It is text-only over the real `crates/**/src/**.rs` tree
//! with comments stripped, the same shape and the same stripper as
//! `crates/vike-boot/tests/one_owner.rs` — the only kind of gate that has held in this repo.
//!
//! # The roster is DERIVED, not written down
//!
//! A composition root is whatever calls [`BOOT_CALL`], exactly as `one_owner.rs` derives it, so a
//! SIXTH root joins this gate the moment it boots and has to be classified before it can merge.
//! Every prose roster in this workspace has rotted; `ci_crates`, `release.yml`'s crate list and the
//! settings registry are the ones the root `CLAUDE.md` names.
//!
//! # What a [`NO_BOOT_ANCHOR`] row is, and what it is not
//!
//! It is an ARGUED exemption in the same spirit as `BootSpec`'s `RemovedEnv::Ignore` /
//! `SettingsLoad::Skip` / `Credentials::Deferred` arms: a root's declaration of what it does at
//! startup, where a diff can see it. It is NOT a debt list — there is no expectation that it
//! drains, and two of the three rows below argue that writing an anchor would make the ledger say
//! something FALSE rather than merely noisy.
//!
//! # The declared limitation
//!
//! It sees a CALL, not a call that runs. A root that calls `journal_boot_settings` inside a branch
//! nothing reaches would pass here. That is the same limitation `one_owner.rs` accepts for the same
//! reason (a text gate cannot evaluate a program), and the behaviour half is covered by
//! `tests/boot_journal.rs`, which drives the real function against a real tree.

use std::path::{Path, PathBuf};

/// The call that MAKES a crate a composition root — the anchor the roster is derived from.
/// Verbatim from `one_owner.rs`'s `BOOT_CALL`, deliberately: the two gates must key on the same
/// definition of "a root", or one of them is checking a different population from the other.
const BOOT_CALL: &str = "vike_boot::boot(";

/// The call that writes the anchor.
const ANCHOR_CALL: &str = "journal_boot_settings(";

/// Composition roots that write NO boot anchor, each with the argument.
///
/// ⚠ Read the reasons: only the first is about RATE. The other two are about TRUTH — a
/// `boot_settings` record claims the ceilings that are EFFECTIVE, and a process that enforces none
/// of them (or that never loaded them) would be putting a false claim into an append-only ledger,
/// which is worse than a gap.
const NO_BOOT_ANCHOR: &[(&str, &str)] = &[
    (
        "crates/vike-cli",
        "`resolve_policy` runs for EVERY subcommand, `secrets path` and `config show` included, so \
         an anchor per invocation would make the ledger's growth track how often a human or an MCP \
         client types a command — the per-invocation-stream-in-a-per-change-ledger shape \
         `vike_model::change_journal`'s module doc refuses for connectivity events. And it would \
         be untrue: this binary mounts no venue, so `market_slippage` and `halt_admit` are \
         effective for nothing in it and `max_notional_per_order` is an advisory guardrail on two \
         surfaces. What is lost — a box where only `vike-cli` ever runs has no bracket at all — is \
         stated at the site.",
    ),
    (
        "crates/vike-recorder",
        "it signs no orders: it mounts no `ExecutionClient`, holds no book and can place nothing, \
         so not one of the ceilings is effective in this process and a record claiming otherwise \
         would be a fabrication in an append-only file. It also declares `SettingsLoad::Skip` — it \
         DISCLOSES the settings without consuming any — so the values it would write are the \
         compiled-in defaults rather than anything it read and obeyed. A box running the recorder \
         beside `vike-tradehub` is already bracketed by that daemon's anchors, off the same \
         `policy.toml`.",
    ),
    (
        "crates/vike-datahub",
        "it loads no settings at all (`SettingsLoad::Skip`, and its store root and address come \
         from its own variables), so its `Policy` is `Policy::default()` — an anchor from here \
         would report the compiled-in defaults as though a file had been read, which is precisely \
         the reading a bracket must never give. It mounts no venue either.",
    ),
];

/// Workspace root, resolved from `CARGO_MANIFEST_DIR` (never CWD) — the idiom every text gate in
/// this repo uses.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// Every `.rs` file under `crates/**/src/`, as `(repo-relative path, text)`.
fn sources() -> Vec<(String, String)> {
    let root = repo_root();
    let mut out = Vec::new();
    walk(&root.join("crates"), &root, &mut out);
    out.sort();
    out
}

fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            // `target/` in a crate directory, and the two vendored trees the root manifest excludes.
            if matches!(name, "target" | "vendor" | "protogen") {
                continue;
            }
            walk(&path, root, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            if !rel.contains("/src/") {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path) {
                out.push((rel, text));
            }
        }
    }
}

/// Line comments removed, so a `//` note NAMING one of these calls is prose rather than a call.
///
/// Load-bearing here in BOTH directions: `crates/vike-cli/src/lib.rs` explains its exemption in a
/// comment that names the anchor, and `crates/vike-boot/src/lib.rs`'s own doc names `boot`.
fn strip_comments(text: &str) -> String {
    text.lines()
        .map(|l| match l.find("//") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The `crates/<name>` (or `crates/bridges/<name>`) directory prefix of a repo-relative path.
fn crate_dir(rel: &str) -> String {
    match rel.split_once("/src/") {
        Some((dir, _)) => dir.to_string(),
        None => rel.to_string(),
    }
}

/// The crates that call [`BOOT_CALL`], `vike-boot` itself excluded — the derived roster.
fn booting_crates(all: &[(String, String)]) -> Vec<String> {
    let mut out: Vec<String> = all
        .iter()
        .filter(|(rel, text)| {
            !rel.starts_with("crates/vike-boot/") && strip_comments(text).contains(BOOT_CALL)
        })
        .map(|(rel, _)| crate_dir(rel))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Does this crate call [`ANCHOR_CALL`] anywhere under its own `src/`?
fn writes_anchor(all: &[(String, String)], dir: &str) -> bool {
    all.iter()
        .filter(|(rel, _)| crate_dir(rel) == dir)
        .any(|(_, text)| strip_comments(text).contains(ANCHOR_CALL))
}

/// **THE gate.** Every derived composition root writes an anchor or carries a row saying why not.
#[test]
fn every_booting_root_either_writes_a_boot_anchor_or_declares_why() {
    let all = sources();
    let exempt: Vec<&str> = NO_BOOT_ANCHOR.iter().map(|(c, _)| *c).collect();

    let mut bad: Vec<String> = Vec::new();
    for dir in booting_crates(&all) {
        if exempt.contains(&dir.as_str()) || writes_anchor(&all, &dir) {
            continue;
        }
        bad.push(format!("  {dir}"));
    }
    assert!(
        bad.is_empty(),
        "a composition root runs `{BOOT_CALL}…)` and writes no BOOT ANCHOR, and declares no reason \
         for it:\n{}\n\n\
         The anchor is one JSONL line per process start recording the EFFECTIVE ceilings, with \
         actor origin `boot`. It exists because the other change-journal channels record DELTAS, \
         and a journal of deltas cannot answer \"what was the ceiling on the 14th\" — and because \
         a HAND EDIT of `policy.toml` is a class NOTHING in this tree observes, so two consecutive \
         anchors that disagree are the only evidence there will ever be that something changed.\n\n\
         Two legitimate responses:\n  \
         1. call `vike_boot::journal_boot_settings` with this root's OWN state root (the tree its \
         rolling log is in — NOT necessarily `Booted::state_dir`; `vike-tradehub`'s \
         `$VIKE_STATE_ROOT` relocates the whole thing) and its resolved `Policy`;\n  \
         2. if this root must not write one, add it to NO_BOOT_ANCHOR with the argument — and if \
         the argument is that the record would be UNTRUE (this root enforces no ceiling, or loaded \
         no settings), say so, because that is a stronger reason than noise and a future reader \
         needs to know which kind it is.\n\
         Deleting the boot call to make the finding disappear is neither.",
        bad.join("\n")
    );
}

/// A row that no longer applies is deleted, not left to rot — the both-directions discipline
/// `one_owner.rs`'s `no_stale_exemptions` applies to its own tables.
#[test]
fn no_stale_exemption() {
    let all = sources();
    let booting = booting_crates(&all);
    let mut stale: Vec<String> = Vec::new();
    for (dir, why) in NO_BOOT_ANCHOR {
        if !booting.contains(&dir.to_string()) {
            stale.push(format!(
                "  {dir} — no longer runs `{BOOT_CALL}…)`, so it is not a composition root and \
                 needs no exemption ({why})"
            ));
            continue;
        }
        if writes_anchor(&all, dir) {
            stale.push(format!(
                "  {dir} — now writes an anchor after all; delete the row rather than leaving an \
                 excuse beside working code ({why})"
            ));
        }
    }
    assert!(stale.is_empty(), "NO_BOOT_ANCHOR rows that no longer apply:\n{}", stale.join("\n"));
}

/// An excuse has to be an ARGUMENT. A blank or `TODO` row is how a root with no anchor and no
/// defence gets waved through — the exact shape `crates/vike-config/tests/policy_is_consumed.rs`
/// gates on its own `Consumed::No` rows.
#[test]
fn every_exemption_states_a_reason() {
    for (dir, why) in NO_BOOT_ANCHOR {
        assert!(
            why.len() > 80 && !why.to_lowercase().contains("todo"),
            "{dir}'s NO_BOOT_ANCHOR row does not say why it writes no boot anchor: {why:?}"
        );
    }
}

/// A floor, not a count: this gate is textual, so a stripper or a walker that quietly stopped
/// matching anything would pass every assertion above by seeing nothing at all.
///
/// ⚠ Each assertion below is keyed on something INDEPENDENT of the thing under test. A floor keyed
/// on `ANCHOR_CALL` itself would go quiet in exactly the mutation this file exists to catch —
/// somebody deleting the root calls — and report a pass instead of a failure.
#[test]
fn the_gate_actually_sees_the_tree() {
    let all = sources();
    assert!(all.len() >= 400, "only {} source files walked — the walker is broken", all.len());

    // The roster anchor still matches: five composition roots run the sequence.
    let booting = booting_crates(&all);
    assert!(
        booting.len() >= 5,
        "only {} crate(s) found calling `{BOOT_CALL}` ({booting:?}) — five composition roots run \
         the startup sequence, so a smaller number means the anchor no longer matches real code \
         and this gate is classifying nobody",
        booting.len()
    );

    // …and every declared exemption names a crate that really is one of them, so the table cannot
    // be satisfied by rows for crates the roster never contained.
    for (dir, _) in NO_BOOT_ANCHOR {
        assert!(
            booting.contains(&dir.to_string()),
            "NO_BOOT_ANCHOR names {dir}, which the derived roster does not contain: {booting:?}"
        );
    }

    // The function the roots are required to call must EXIST, and it is defined in this crate. Keyed
    // on the DEFINITION rather than on a call site, so a deleted call site cannot make this quiet.
    let (_, boot_lib) = all
        .iter()
        .find(|(p, _)| p == "crates/vike-boot/src/lib.rs")
        .expect("vike-boot's lib must be walked");
    assert!(
        strip_comments(boot_lib).contains("pub fn journal_boot_settings"),
        "`vike_boot::journal_boot_settings` is gone — either it was renamed (update ANCHOR_CALL) \
         or the boot anchor was removed, and this gate has been checking a call to nothing"
    );

    // The stripper is doing its job in the direction that matters: `crates/vike-cli/src/lib.rs`
    // NAMES the anchor in the comment that argues its exemption, and must still not count as a
    // call. Keyed on the exempt crate's text rather than on any root's wiring.
    let (_, cli) = all
        .iter()
        .find(|(p, _)| p == "crates/vike-cli/src/lib.rs")
        .expect("vike-cli's lib must be walked");
    assert!(
        cli.contains("journal_boot_settings"),
        "vike-cli no longer explains why it writes no anchor at the site — the comment that names \
         it is what makes the stripper check below meaningful"
    );
    assert!(
        !strip_comments(cli).contains(ANCHOR_CALL),
        "the comment stripper has stopped working: vike-cli's prose mention of the anchor is \
         being read as a call, which would let this gate pass a root that only TALKS about writing \
         one"
    );
}
