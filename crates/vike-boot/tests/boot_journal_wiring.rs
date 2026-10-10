//! **Every composition root either writes a BOOT ANCHOR or declares why it does not.**
//!
//! # Why a gate rather than a convention
//!
//! `vike_boot::journal_boot_settings` is the whole of the anchor's behaviour and
//! `tests/boot_journal.rs` gates it end to end, against a real directory tree. **None of that
//! notices if no root ever calls it** — and `vike_model::change_journal`'s `boot_settings` record
//! type once shipped BUILT AND TESTED and called by nothing.
//!
//! So this file asks the other question, text-only over the real `crates/**/src/**.rs` tree with
//! comments stripped. It shares `common/mod.rs` with `one_owner.rs` — the same walk, the same
//! stripper and the same derived roster ([`booting_crates`]) — so a new root joins both gates the
//! moment it boots and has to be classified here before it can merge.
//!
//! # What a [`NO_BOOT_ANCHOR`] row is, and what it is not
//!
//! It is an ARGUED exemption in the same spirit as `BootSpec`'s `RemovedEnv::Ignore` /
//! `SettingsLoad::Skip` / `Credentials::Deferred` arms: a root's declaration of what it does at
//! startup, where a diff can see it. It is NOT a debt list — there is no expectation that it
//! drains, and the rows argue that writing an anchor would make the ledger say something FALSE
//! rather than merely noisy.
//!
//! # The declared limitation
//!
//! It sees a CALL, not a call that runs: a root that calls `journal_boot_settings` inside a branch
//! nothing reaches would pass here. A text gate cannot evaluate a program (`one_owner.rs` accepts the
//! same limitation); the behaviour half is `tests/boot_journal.rs`.

mod common;

use common::{BOOT_CALL, booting_crates, crate_dir, sources, strip_comments};

/// The call that writes the anchor.
const ANCHOR_CALL: &str = "journal_boot_settings(";

/// Composition roots that write NO boot anchor, each with the argument.
///
/// ⚠ Read the reasons: only the first is about RATE. The other is about TRUTH — a
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
        "crates/vike-datahub",
        "it mounts no venue: a `--record` start subscribes to public venue feeds and writes a \
         tape, but it mounts no `ExecutionClient`, holds no book and can place nothing, so not one \
         of the ceilings is effective in this process and a record claiming otherwise would be a \
         fabrication in an append-only file. A box running it beside `vike-tradehub` is already \
         bracketed by that daemon's anchors, off the same `policy` rows.",
    ),
];

/// Does this crate call [`ANCHOR_CALL`] anywhere under its own `src/`? (A test-module FILE is not
/// read: [`sources`] drops it.)
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
         a HAND EDIT of a settings row is a class NOTHING in this tree observes, so two consecutive \
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

    // The roster anchor still matches: four composition roots run the sequence. A `>=`, so a FIFTH
    // root joining does not touch it — only a root LEAVING does, which is the edit that has to be
    // deliberate because it is indistinguishable from the anchor having stopped matching.
    let booting = booting_crates(&all);
    assert!(
        booting.len() >= 4,
        "only {} crate(s) found calling `{BOOT_CALL}` ({booting:?}) — four composition roots run \
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
    let (_, boot_anchor) = all
        .iter()
        .find(|(p, _)| p == "crates/vike-boot/src/boot_anchor.rs")
        .expect("vike-boot's boot_anchor.rs must be walked");
    assert!(
        strip_comments(boot_anchor).contains("pub fn journal_boot_settings"),
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
