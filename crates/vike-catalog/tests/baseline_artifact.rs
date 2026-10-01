//! **THE COMMITTED BASELINE ARTIFACT IS VALIDATED ON EVERY PR** — which is what lets
//! `.github/workflows/release.yml` ATTACH those bytes unchanged instead of rendering them.
//!
//! # Why a copy at tag time rather than a render
//!
//! The precedent is `profile.json`, and release.yml argues it at its own `cp` line: that file's
//! in-process gate compares the render to the committed bytes on every PR, "so publishing the
//! fixture publishes what the gate already checked". The same argument holds here for a stronger
//! reason — **there is nothing to render FROM.** `docs/decisions/0066`'s decision 8: producing a
//! baseline needs an authenticated venue fetch, no runner may hold those credentials
//! (`scripts/refuse_live_credentials.sh` refuses such a store outright on both live-smoke lanes),
//! and so the fetched bytes ARE the source. That is the shape of `fixtures/r0..r6/`, whose
//! committed bytes ARE the oracle because no exporter survives, and of the vendored IBKR client,
//! committed precisely because CI cannot re-derive it.
//!
//! So this file is the half that makes the copy honest: it runs
//! `vike_catalog::BaselineCatalog::parse` — the SAME function the desktop runs at startup, never a
//! second validator — over the real committed file, on every PR, in the derived roster lane.
//!
//! ⚠ **The rule the tree's `commit-the-pin-ship-the-build` law actually asks for is satisfied
//! rather than broken**, and the record says so by name: the pin is committed and the build is
//! shipped; what is unusual is only that here the pin IS the data. What it costs, declared: a
//! release renderer that reads a committed DATA FILE is wider than one that reads a compile-time
//! table, and `assets/` is on `scripts/publish_mirror.sh`'s ALLOW list so the file is public.
//!
//! ⚠ It reads the file at RUN time rather than `include_str!`ing it, and that is deliberate on two
//! counts. An `include_str!` would bake the whole instrument list into every binary linking
//! `vike-catalog` — the exact opposite of shipping it as a release asset the deployed binary FINDS
//! — and it would make the public mirror's build depend on the file rather than merely carry it.
//! A missing file SKIPS loudly, the shape `crates/vike-core`'s latency pin uses for
//! `.github/workflows/ci.yml`.

use std::path::{Path, PathBuf};

use vike_catalog::{BASELINE_SOURCE, BaselineCatalog};

fn repo_root() -> PathBuf {
    // `CARGO_MANIFEST_DIR` is `<root>/crates/vike-catalog`.
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap().to_path_buf()
}

/// **The committed artifact parses under every rule the loader applies at run time.**
///
/// ⚠ Mutation proof: break one rule in the file — drop a row's `fetched`, name `ctrader`, give a
/// row a qualifier that is not alpaca's environment — and this goes red naming the rule, because it
/// calls the PRODUCTION parser rather than re-stating its rules.
#[test]
fn the_committed_baseline_parses_under_the_production_rules() {
    let path = repo_root().join(BASELINE_SOURCE);
    let Ok(bytes) = std::fs::read(&path) else {
        eprintln!(
            "SKIPPING: {} is absent. That is expected ONLY in the public mirror, which publishes \
             `assets/` but is built from a snapshot; in this workspace it means the committed \
             baseline was deleted and every venue it covered silently lost its list.",
            path.display()
        );
        return;
    };
    let doc = BaselineCatalog::parse(&bytes)
        .unwrap_or_else(|e| panic!("{} is not a valid baseline: {e}", path.display()));

    // The `note` is for whoever opens the file; it is read by no code, so the one thing worth
    // asserting is that it has not been emptied into a file nobody can interpret.
    assert!(
        doc.note.len() > 80,
        "the committed baseline's `note` is what tells the next author how a row gets there — an \
         undocumented data file is how a shipped list acquires a row nobody can date"
    );

    // ⚠ NO assertion on the CONTENT, and that is decision 8's last consequence stated as a test:
    // "Nothing here ratifies the baseline's CONTENT. A list fetched once and committed is a claim
    // about a venue on a date." An assertion that alpaca has N instruments would be this file
    // ratifying a claim it cannot check, and would go red on the next honest re-fetch.
    for row in &doc.venues {
        assert!(!row.fetched.is_empty(), "parse() should have refused this already");
        eprintln!(
            "baseline: {} — {} instruments, fetched {}, qualifier `{}`",
            row.venue,
            row.instruments.len(),
            row.fetched,
            row.qualifier
        );
    }
}

/// **The reader can fail** — otherwise the test above is a way of parsing nothing.
///
/// The rule `crates/vike-ops`' own gates keep re-learning: a parser whose failure path is never
/// exercised turns every assertion built on it green. This plants each refusal the artifact is
/// validated against and requires the production parser to produce it.
#[test]
fn the_validator_actually_refuses_the_things_it_claims_to() {
    // An undated row.
    assert!(
        BaselineCatalog::parse(
            br#"{"venues":[{"venue":"alpaca","fetched":"","qualifier":"demo","instruments":[]}]}"#
        )
        .is_err(),
        "an undated row must refuse"
    );
    // A venue that may never ship one.
    assert!(
        BaselineCatalog::parse(
            br#"{"venues":[{"venue":"ctrader","fetched":"2026-09-16","qualifier":"x","instruments":[]}]}"#
        )
        .is_err(),
        "ctrader must refuse"
    );
    // A qualifier alpaca's measured axis does not admit.
    assert!(
        BaselineCatalog::parse(
            br#"{"venues":[{"venue":"alpaca","fetched":"2026-09-16","qualifier":"paper","instruments":[]}]}"#
        )
        .is_err(),
        "alpaca's environment set is closed"
    );
    // …and the honest case parses, so the three above are refusals rather than a parser that
    // refuses everything.
    BaselineCatalog::parse(
        br#"{"venues":[{"venue":"alpaca","fetched":"2026-09-16","qualifier":"demo","instruments":[]}]}"#,
    )
    .expect("a stamped, qualified, eligible row must parse");
}
