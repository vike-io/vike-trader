//! **Every path `crates/bridges/fxcm/build.rs` STATS must be a path cargo WATCHES.**
//!
//! The build script decides stub-versus-linked by asking the filesystem whether the ForexConnect
//! SDK is there. Cargo caches a build script's verdict and re-runs it only when a declared input
//! changes — so a probed path that is not also a declared input means the verdict outlives the
//! fact it was derived from.
//!
//! ## The incident this was written from
//!
//! Measured 2026-08-25 on the CI box lane `vike-fresh3`. That lane carried a `vendor/fcsdk` symlink; it
//! was removed, and cargo kept the cached LINKED verdict because nothing it watched had changed.
//! Every branch checked out in that lane then failed with `rust-lld: error: unable to find library
//! -lForexConnect` — a link error attributable to no branch, which cost two agents a round of
//! diagnosis each. `cargo clean -p vike-fxcm` cleared it, which is the tell that the input was
//! untracked rather than wrong.
//!
//! The reverse direction is the quieter one and the reason this is a gate rather than a fix note:
//! staging the SDK onto a box that has already built the stub leaves
//! `crates/bridges/fxcm/src/lib.rs`'s `sdk_available` false, so the start (`VenueAdapterFxcm` in
//! `crates/bridges/fxcm/src/adapter.rs`) refuses the live start on a box the operator has just
//! finished preparing.
//! That refusal is correct — it is the safe outcome the build script's own macOS argument spells
//! out — but the operator is owed the rebuild rather than the refusal.
//!
//! ## What this checks, and what it deliberately does not
//!
//! It reads the build script's SOURCE, because a build script cannot be called from a test — the
//! same constraint `crates/bridges/fxcm/tests/fcsdk_packaging.rs` and
//! `crates/bridges/fxcm/tests/fcsdk_rpath_tag.rs` both work around, and the harvest idiom is
//! theirs.
//!
//! ⚠ `fcsdk_rpath_tag.rs`'s doc is a warning against exactly this shape: its sibling passed for
//! the entire time the rpath mechanism was broken, because both halves agreed on a spelling while
//! the loader ignored the result. So state the limit plainly rather than implying coverage this
//! does not have. **This gate proves a DECLARATION, not a behaviour.** It cannot observe cargo
//! honouring `rerun-if-changed`; that is cargo's documented contract and not this workspace's to
//! verify. What makes it worth having anyway is that the failure it guards is an OMISSION — a
//! probe added with no matching declaration — and an omission is exactly what a source-level
//! reading can see. The rpath case was the opposite: a present flag in an ineffective position.
//!
//! Both harvests assert their own non-vacuity, so a rename that empties one fails here instead of
//! passing silently.

use std::collections::BTreeSet;
use std::path::PathBuf;

fn build_script() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("build.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The identifier immediately left of `.` in `<ident>.<call>`, harvested for each `call` given.
///
/// Deliberately identifier-shaped rather than expression-shaped: the build script binds every path
/// it probes to a local first (`header`, `lib`), so a bare identifier is the whole vocabulary, and
/// anything more permissive would start matching prose in this crate's heavily-commented script.
fn receivers_of(src: &str, calls: &[&str]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for call in calls {
        let needle = format!(".{call}");
        for (i, _) in src.match_indices(&needle) {
            let ident: String = src[..i]
                .chars()
                .rev()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            if !ident.is_empty() && !ident.chars().next().unwrap().is_numeric() {
                out.insert(ident);
            }
        }
    }
    out
}

/// The emission itself.
const MARKER: &str = "println!(\"cargo:rerun-if-changed={}\", ";
/// A call of the build script's declaring helper (`crates/bridges/fxcm/build.rs`'s
/// `declare_rerun_input`: the path itself, or its nearest existing parent when it is missing).
const HELPER_CALL: &str = "declare_rerun_input(&";
const HELPER_DEF: &str = "fn declare_rerun_input(";

/// Every identifier the script hands to `cargo:rerun-if-changed={}`, directly or through the helper.
///
/// The `println!(` prefix is load-bearing — it restricts the harvest to actual EMISSIONS, so the
/// long comment block above those lines (which names the directive by name) can never be counted
/// as one. Same reasoning as `fcsdk_rpath_tag.rs`'s `emitted_link_args`. A helper call counts only
/// as a STATEMENT (its line starts with it), for the same reason.
fn declared_dynamic(src: &str) -> BTreeSet<String> {
    let ident = |rest: &str| -> String {
        rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect()
    };
    let mut out = BTreeSet::new();
    for (i, _) in src.match_indices(MARKER) {
        out.insert(ident(&src[i + MARKER.len()..]));
    }
    for line in src.lines() {
        if let Some(rest) = line.trim_start().strip_prefix(HELPER_CALL) {
            out.insert(ident(rest));
        }
    }
    out.remove("");
    out
}

/// False when the helper is called but its body emits nothing: every call would then declare
/// nothing while the harvest above counted it.
fn helper_emits(src: &str) -> bool {
    if !src.lines().any(|l| l.trim_start().starts_with(HELPER_CALL)) {
        return true;
    }
    let Some(start) = src.find(HELPER_DEF) else { return false };
    let body = &src[start..];
    body[..body.find("\n}").unwrap_or(body.len())].contains(MARKER)
}

/// THE GATE: nothing is stat-ed that is not also watched.
#[test]
fn every_probed_path_is_also_a_declared_rerun_input() {
    let src = build_script();

    // Every way this script could reach the filesystem to decide. `exists` is what it uses today;
    // the rest are here so that switching to one of them is not a silent escape from this gate.
    let probed = receivers_of(&src, &["exists()", "try_exists()", "is_file()", "is_dir()"]);
    let declared = declared_dynamic(&src);

    assert!(
        !probed.is_empty(),
        "harvested no filesystem probe from build.rs at all. Either the SDK-present gate is gone \
         (in which case delete this test and say why in the commit) or it now reaches the disk by \
         a call this harvest does not know — add it to the list in `every_probed_path_is_also_a_\
         declared_rerun_input`, because an unknown call is an unwatched one."
    );
    assert!(
        !declared.is_empty(),
        "harvested no `cargo:rerun-if-changed={{}}` emission from build.rs. The probed paths \
         {probed:?} are therefore watched by nothing, and cargo will cache the stub-versus-linked \
         verdict across the change that should flip it — the CI box lane vike-fresh3, 2026-08-25."
    );

    assert!(
        helper_emits(&src),
        "build.rs calls `declare_rerun_input` but its body emits no `cargo:rerun-if-changed`, so \
         every path handed to it is watched by nothing."
    );

    let unwatched: Vec<&String> = probed.difference(&declared).collect();
    assert!(
        unwatched.is_empty(),
        "build.rs stats {unwatched:?} but never declares {unwatched:?} as a \
         `cargo:rerun-if-changed` input.\n\nCargo will keep the previous stub-versus-linked verdict \
         when that path appears or disappears. Staging the SDK then leaves `sdk_available()` false and \
         the live mount refuses on a box that IS prepared; removing it leaves a build that cannot \
         link at all (`unable to find library -lForexConnect`).\n\nFix: declare it beside the \
         others with `declare_rerun_input(&<path>, &home);`, never a bare emission of a path that \
         may be missing: cargo counts a missing declared path as changed on EVERY build, and the \
         re-run recompiles everything that links vike-fxcm.\
         \n\nprobed: {probed:?}\ndeclared: {declared:?}"
    );
}

/// The static half, kept honest separately: the shim source and the env var are watched too, and
/// they are spelled as LITERALS rather than through a binding, so the dynamic harvest above cannot
/// see them and their loss would otherwise be invisible here.
#[test]
fn the_shim_source_and_the_sdk_env_var_stay_watched() {
    let src = build_script();
    for needle in [
        "println!(\"cargo:rerun-if-env-changed=FCSDK_DIR\")",
        "println!(\"cargo:rerun-if-changed=src/shim/fcshim.cpp\")",
    ] {
        assert!(
            src.contains(needle),
            "build.rs no longer emits `{needle}`. `FCSDK_DIR` selects the SDK root outright and \
             the shim is the C++ actually compiled — dropping either caches a verdict across the \
             input that decides it."
        );
    }
}

/// The harvesters' own control. Without this, a bug that made `receivers_of` or `declared_dynamic`
/// answer identically for any input would leave the gate above green forever — the vacuous pass
/// this repo has been bitten by often enough to write down.
#[test]
fn the_harvesters_can_tell_a_watched_probe_from_an_unwatched_one() {
    let watched = r#"
        println!("cargo:rerun-if-changed={}", header.display());
        if !header.exists() { return; }
    "#;
    let unwatched = r#"
        if !header.exists() { return; }
    "#;
    let commented = r#"
        // println! with cargo:rerun-if-changed={} is discussed here but not emitted.
        if !header.exists() { return; }
    "#;

    let probe = ["exists()", "try_exists()", "is_file()", "is_dir()"];
    assert_eq!(receivers_of(watched, &probe), BTreeSet::from(["header".to_string()]));
    assert_eq!(declared_dynamic(watched), BTreeSet::from(["header".to_string()]));
    assert!(
        receivers_of(watched, &probe).difference(&declared_dynamic(watched)).next().is_none(),
        "the watched sample must satisfy the gate"
    );

    assert!(
        receivers_of(unwatched, &probe).difference(&declared_dynamic(unwatched)).next().is_some(),
        "the unwatched sample must VIOLATE the gate — if it does not, the gate proves nothing"
    );
    assert!(
        declared_dynamic(commented).is_empty(),
        "a comment naming the directive is not an emission of it; counting one would let the gate \
         pass on a script that only TALKS about watching its inputs"
    );

    let through_helper = r#"
        declare_rerun_input(&header, &home);
        if !header.exists() { return; }
        fn declare_rerun_input(probe: &Path, home: &Path) {
            let mut watched = probe;
            while !watched.exists() { watched = watched.parent().unwrap(); }
            println!("cargo:rerun-if-changed={}", watched.display());
        }
    "#;
    let hollow_helper = r#"
        declare_rerun_input(&header, &home);
        if !header.exists() { return; }
        fn declare_rerun_input(probe: &Path, home: &Path) {}
    "#;
    let helper_in_prose = r#"
        // declare_rerun_input(&header, &home) is how a missing path gets declared.
        if !header.exists() { return; }
    "#;
    assert!(
        receivers_of(through_helper, &probe)
            .difference(&declared_dynamic(through_helper))
            .next()
            .is_none()
            && helper_emits(through_helper),
        "a path handed to the helper, whose body emits, must satisfy the gate"
    );
    assert!(!helper_emits(hollow_helper), "a helper that emits nothing must fail the gate");
    assert!(
        declared_dynamic(helper_in_prose).is_empty(),
        "a comment naming the helper is not a call of it"
    );
    assert!(helper_emits(watched), "a script that never calls the helper owes it nothing");
}
