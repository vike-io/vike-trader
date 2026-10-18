//! **A HELD reconcile divergence must reach the LOG, not only the in-memory ring — and it must do
//! so exactly ONCE per divergence.**
//!
//! `CoreThread`'s alert loop calls `self.note(..)`, which writes a ring buffer reachable only
//! through the control channel — and that channel is OFF on the shipped daemon unless an operator
//! turns it on. Measured on the live the CI box box on 2026-08-24: `reconcile pass folded venue=bybit
//! events=0 alerts=2` every 60 seconds for hours, ZERO lines at WARN or above, and no way for
//! anyone reading logs to learn which two divergences were being held. The COUNT reached the
//! operator and the CONTENT did not.
//!
//! ⚠ **The second half of that sentence is newer than the first, and it is why these anchors
//! moved.** Emitting the line unconditionally fixed the silence and bought a flood: under
//! `VIKE_RECONCILE_POLICY=quarantine` a divergence nothing heals is re-diffed and re-held every
//! interval, so the CI box logged 396 identical WARN lines on 2026-08-25 and 345 on 2026-08-24 — every
//! one of them the same bybit `PositionOnlyExternal`, with a genuinely NEW divergence indis-
//! tinguishable among them. The emission therefore moved behind
//! `crates/vike-core/src/runtime/recon_held.rs`'s `HeldAnnouncer`, which logs the TRANSITIONS of
//! the held set (entered / cleared) plus a slow backlog summary. Both properties are gated below:
//! the raise path still logs, and no warn on that path is unguarded.
//!
//! A text gate rather than a runtime one, and the reason is worth stating: the one tracing capture
//! `vike-core` has (`tracing-test`, in the white-box `runtime/tests/recon_held.rs`) sees the
//! announcer's lines in the scenarios it builds, not where the raise site sits, and building a
//! subscriber for every raise path to assert one line would cost more than the line is worth. This
//! is the same shape as `crates/vike-tradehub/tests/live_lock_claim_order_gate.rs` — it checks
//! the SOURCE, and its own mutation test is what makes it a gate rather than a decoration.
//! (The BEHAVIOUR of the announcer — once per identity, again on a change, a summary on its own
//! cadence — is unit-tested where it lives, in `recon_held.rs`'s own `mod tests`.)

use std::path::PathBuf;

fn read(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(rel)
        .canonicalize()
        .unwrap_or_else(|e| panic!("{rel} exists: {e}"));
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn runtime_src() -> String {
    read("src/runtime/reconcile.rs")
}

fn recon_held_src() -> String {
    read("src/runtime/recon_held.rs")
}

/// The raise path emits a log line, and it does so BEFORE the alert is filed — so a held divergence
/// is visible even if the row is later refreshed, confirmed or dropped.
///
/// ⚠ Anchored on the ORDER, not on mere presence: a log call anywhere in the 4 000-line file would
/// satisfy a presence check while the raise path stayed silent.
#[test]
fn a_held_divergence_is_logged_where_the_alert_is_raised() {
    let src = runtime_src();

    let note = src
        .find(r#"RECON alert #{id} {:?}: {} (awaiting confirm)"#)
        .expect("the ring note at the alert-raise site still exists");
    let insert = src[note..]
        .find("self.recon_alerts.insert(")
        .map(|i| note + i)
        .expect("the alert is filed just below the note");

    let between = &src[note..insert];
    assert!(
        between.contains("recon_held::warn_newly_held("),
        "the alert-raise path files a held divergence without logging it. The ring `note` is \
         reachable only through the control channel, which is OFF on the shipped daemon — so an \
         operator would see `alerts=2` and have no way to learn WHICH two. Measured live on \
         the CI box, 2026-08-24."
    );
}

/// ...and the line it emits is a real WARN carrying the divergence KIND.
///
/// Split from the test above because the emission moved into a named helper: the raise SITE is a
/// property of `runtime/reconcile.rs`, the line's SHAPE is a property of `recon_held.rs`, and checking
/// each where it lives is what keeps both honest.
#[test]
fn the_logged_line_is_a_warn_carrying_the_divergence_kind() {
    let src = recon_held_src();
    let start = src
        .find("pub(crate) fn warn_newly_held(")
        .expect("the held-divergence emitter still exists");
    let body = src[start..].split("\n}\n").next().expect("the fn has a body");

    assert!(
        body.contains("tracing::warn!"),
        "a held divergence needs an operator, so it must be WARN — an INFO line is filtered out \
         by the default console level on the shipped daemon"
    );
    assert!(
        body.contains("kind = ?id.kind"),
        "the line must carry the divergence KIND — `alerts=2` was already in the log and is \
         exactly the thing that told nobody anything"
    );
}

/// ...and EVERY such warn on the reconcile path is guarded by the announcer.
///
/// This replaces an older `the_refresh_path_does_not_log`, which asserted the absence of a
/// `tracing::warn!` in the dedup branch. That was the right property expressed through the wrong
/// mechanism: what must never happen is a warn per PASS, and now that the raise path itself is
/// conditional, "which branch it sits in" no longer decides that — the `announce` guard does. So
/// the gate is over every call site rather than over one branch, which is strictly stronger: it
/// covers the raise path too, where the flood actually came from.
///
/// A recurring divergence re-raises every pass — 1 440 times a day on the default 60 s cadence.
/// Logging each one is how an operator learns to filter the whole channel out, which costs more
/// than the silence does.
#[test]
fn every_held_divergence_warn_is_guarded_by_the_announcer() {
    let src = runtime_src();

    let sites = warn_call_sites(&src);
    assert!(
        !sites.is_empty(),
        "no held-divergence warn call site at all — the gate above should have caught this first"
    );

    for site in sites {
        let window = guard_window(&src, site);
        assert!(
            window.contains("if announce {"),
            "an UNGUARDED held-divergence warn. Every one must sit behind the announcer's \
             `announce` verdict, or a divergence held for a day writes 1 440 identical WARN lines \
             and an operator who filters those also filters the ones that matter. Measured on \
             the CI box 2026-08-25: 396 lines in a day for ONE bybit PositionOnlyExternal. Context:\n{window}"
        );
    }
}

/// The module-qualified spelling every real call site uses today.
const QUALIFIED_WARN: &str = "recon_held::warn_newly_held(";

/// Every call site of the held-divergence emitter in `src`, as the byte offset where the call's
/// spelling starts: [`QUALIFIED_WARN`], plus every name the file's own `use` declarations bind the
/// function to.
///
/// ⚠ **The imported spellings are the repair for a measured hole** (2026-10-05). This matched
/// [`QUALIFIED_WARN`] only, so `use crate::runtime::recon_held::warn_newly_held;` and a bare
/// `warn_newly_held(…)` with no `if announce {` above it was not a site at all — green, with the
/// 396-lines-a-day flood restored (`scripts/run_mutations.sh`'s row B7). `vike_model::scan`'s
/// `imported_spellings` is the shared resolver `paper_mount_arming_gate.rs` already uses for the
/// same class one type over, so no second resolver is written here. A bare name is searched for
/// only when the file imports it, which is what keeps an unrelated local `fn warn_newly_held` from
/// counting; a match preceded by an identifier byte or `:` is the tail of a longer path and is
/// left to the spelling that path IS.
fn warn_call_sites(src: &str) -> Vec<usize> {
    let mut needles = vec![QUALIFIED_WARN.to_string()];
    needles.extend(
        vike_model::scan::imported_spellings(src, "warn_newly_held").into_iter().map(|s| s + "("),
    );
    let bytes = src.as_bytes();
    let mut ends = std::collections::BTreeMap::new();
    for needle in &needles {
        for (at, _) in src.match_indices(needle.as_str()) {
            let tail_of_a_path = needle.as_str() != QUALIFIED_WARN
                && at > 0
                && (bytes[at - 1].is_ascii_alphanumeric() || matches!(bytes[at - 1], b'_' | b':'));
            if !tail_of_a_path {
                // Keyed on where the call's `(` sits, so two spellings of ONE call count once.
                ends.entry(at + needle.len()).or_insert(at);
            }
        }
    }
    ends.into_values().collect()
}

/// The text just above a call site, where its guard must be: the statement immediately above the
/// call. A generous window keeps the check robust to reformatting without letting an unrelated
/// `if` several statements up count. Widened forward to a char boundary, so a multi-byte
/// character 160 bytes up cannot panic the slice.
fn guard_window(src: &str, site: usize) -> &str {
    let mut from = site.saturating_sub(160);
    while !src.is_char_boundary(from) {
        from += 1;
    }
    &src[from..site]
}

/// [`warn_call_sites`] and [`guard_window`] on planted sources — the mutation self-test, including
/// the exact shape row B7 plants.
#[test]
fn the_call_site_finder_sees_every_spelling_it_claims_to() {
    let guarded = "if announce {\n    recon_held::warn_newly_held(id, &ident, &d);\n}\n";
    let sites = warn_call_sites(guarded);
    assert_eq!(sites.len(), 1, "the qualified spelling is a site");
    assert!(guard_window(guarded, sites[0]).contains("if announce {"));

    // Row B7: the function imported, then called bare with no guard.
    let imported = "mod m {\n    use crate::runtime::recon_held::warn_newly_held;\n    \
                    pub fn raise(id: u64) {\n        warn_newly_held(id, \"x\", \"y\");\n    }\n}\n";
    let sites = warn_call_sites(imported);
    assert_eq!(sites.len(), 1, "an imported bare call is a site");
    assert!(!guard_window(imported, sites[0]).contains("if announce {"), "…and it is unguarded");

    // A module alias renames the path, not the function.
    let aliased =
        "use crate::runtime::recon_held as rh;\nfn f() { rh::warn_newly_held(1, \"\", \"\"); }\n";
    assert_eq!(warn_call_sites(aliased).len(), 1, "a module-aliased call is a site");

    // One call, both spellings in scope: counted once, not twice.
    let both = "use crate::runtime::recon_held::warn_newly_held;\n\
                if announce {\n    recon_held::warn_newly_held(id, &i, &d);\n}\n";
    assert_eq!(warn_call_sites(both).len(), 1, "the qualified call is not ALSO a bare one");

    // A bare call with no import is some other function, and stays out.
    assert!(warn_call_sites("fn f() { warn_newly_held(1); }\n").is_empty());

    // The window never splits a multi-byte character.
    let wide = format!("{}{guarded}", "⚠".repeat(60));
    let site = warn_call_sites(&wide)[0];
    assert!(guard_window(&wide, site).contains("if announce {"));
}
