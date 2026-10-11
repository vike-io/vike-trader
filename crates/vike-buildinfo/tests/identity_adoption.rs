//! Every binary that prints a `--version` line states its BUILD IDENTITY — as a gate, not a habit.
//!
//! # Why this is a gate
//!
//! The claim "our binaries say which commit they came from" is worth nothing the first time one of
//! them does not, and that binary is invariably the one being installed on a box at 2am. It is also
//! exactly the shape this repo has been burned by repeatedly — a roster written in prose, true on
//! the day it was written, silently false a month later (`ci_crates`, `release.yml`'s crate list,
//! the settings registry before `crates/vike-ops/tests/settings_secrets/settings_registry.rs`). So the roster is
//! DERIVED here: walk the real `crates/` tree, find every `--version` printer, and require each one
//! to go through `vike_buildinfo::version_line` or to be a declared row in [`WITHOUT_IDENTITY`]
//! with its reason.
//!
//! # How a printer is recognised
//!
//! A `println!` under some `crates/**/src/**.rs` whose own ARGUMENTS name the package version.
//! Text-only, comments stripped — the same mechanism `crates/vike-ops/tests/settings_secrets/settings_registry.rs`
//! uses, and the only kind of gate that has ever held in this repo. It deliberately does NOT match
//! every mention of `CARGO_PKG_VERSION`: `crates/vike-cli/src/cmd/mcp.rs`'s `SERVER_VERSION` is an
//! MCP protocol field and `crates/vike-mount/src/incident/manifest.rs`'s `pkg_version` is a report field;
//! neither is a `--version` answer and neither should be forced to carry one.
//!
//! "Names the package version" resolves ONE indirection, crate-wide: a `const`/`static` bound to
//! `env!("CARGO_PKG_VERSION")` counts as the version wherever that name is printed
//! ([`version_aliases`]). A `--version` arm written `const V: &str = env!("CARGO_PKG_VERSION");
//! println!("{NAME} {V}")` is otherwise INVISIBLE to a literal-only sweep, and the
//! `printers.len() >= 5` floor cannot notice one missing printer. This is the same indirection
//! `settings_registry.rs` resolves for its `const *_ENV: &str` rows and for the same reason —
//! crate-wide rather than file-local, because the constant routinely lives in a sibling file.
//!
//! # Declared blind spots
//!
//! One indirection, and text only. A version reached through a FUNCTION (`fn v() -> &'static str`),
//! assembled by `concat!`/`format!` into a name this sweep never sees, or printed by a macro of our
//! own would still slip through. That is stated rather than tolerated silently: closing it needs a
//! real resolver, and the honest position is that this gate catches the shapes a `--version` arm is
//! actually written in today. `crates/vike-ops/tests/settings_secrets/settings_registry.rs` takes the same line — a
//! blind spot has to be declared to exist.
//!
//! The NAME check ([`every_version_printer_prints_its_own_package_name`]) reads the printing CALL,
//! not who calls it, so it cannot see:
//!
//!   * a printer in a LIBRARY crate that another package's binary calls. It spells
//!     `env!("CARGO_PKG_NAME")` correctly and passes here, yet expands to the LIBRARY's name — a
//!     `--version` moved into a shared helper crate is exactly that shape;
//!   * which binary a printer belongs to at all. That `./vike-backend trade` reaches
//!     `crates/vike-tradehub/src/tradehub_cli.rs` (package `vike-tradehub`) is read from
//!     `crates/vike/src/main.rs`'s `TOOLS`, not derived; `RELEASE_PRINTERS` only pins that the three
//!     printers the release reaches are among those checked;
//!   * a printer this module does not RECOGNISE as one — a `version_line` bound with `let` and
//!     printed by name, a `print!`, a `writeln!`, a macro of our own. Outside `RELEASE_PRINTERS` such
//!     a printer FAILS OPEN: a wrong name in it passes. Inside them it fails closed, because the pin
//!     then finds no checked name in the file. `RELEASE_PRINTERS` is a hand list: a new identity
//!     target added to `.github/workflows/release.yml` does not join it by itself.
//!
//! It fails SAFE on a name bound through a `const` (`version_line(NAME, …)`): refused here, though
//! correct, rather than passed and refused at tag time.
//!
//! # Why it lives HERE and not in vike-ops
//!
//! `vike-ops` owns the repo-walking gates, and this one could have gone there. It did not, for one
//! reason: this gate is about THIS crate's ADOPTION, so the crate that would go stale and the gate
//! that catches it stay in the same directory — and vike-ops would have had to grow a dependency on
//! vike-buildinfo to say anything about it.

#[path = "identity_adoption/printers.rs"]
mod printers;
#[path = "identity_adoption/scan.rs"]
mod scan;
#[path = "identity_adoption/scanner_selftests.rs"]
mod scanner_selftests;

use printers::{OWN_PACKAGE_NAME, version_line_names, version_printer_calls, version_printers};
use scan::repo_root;

/// `--version` printers that do NOT state their build identity, each with the reason.
///
/// A row here is a REAL gap — a binary whose `--version` cannot answer "which commit is this?" —
/// not a false positive being silenced. [`no_stale_exemptions`] fails when a row's file stops
/// existing OR starts using `version_line`, so closing one is a one-line deletion.
const WITHOUT_IDENTITY: &[(&str, &str)] = &[
    (
        "crates/vike-backtest/src/backtest_cli.rs",
        "vike-backtest is a LIBRARY at layer 50 with vike-datahub, vike-studio and \
         vike-report above it. A normal dependency on vike-buildinfo would make every commit \
         rebuild the simulator and everything stacked on it, because this crate's build script \
         reruns whenever HEAD moves — the cost lands on the inner loop, not on the one bin that \
         would gain a line. Every crate wired today is top-of-graph, where the cost is a relink.",
    ),
    (
        "crates/vike-backfill/src/cli.rs",
        "vike-backfill is in `xtask/src/ci/tables/roster.rs`'s EXCLUDE_FROM_CI, so NOTHING in CI compiles \
         it and neither `just windows-check` nor `just the build runner` covers it. Wiring it would be \
         one line that no gate on this box or any other could prove compiles. Its bins are also \
         batch tools run by hand, not daemons installed on a trading box — the incident class this \
         crate exists for.",
    ),
];

/// The printers `.github/workflows/release.yml`'s identity call reaches: `./vike-backend` (the
/// dispatcher's own `Route::Version`), `"./vike-backend trade"` (the `trade` row's
/// `vike_tradehub::tradehub_cli::run`, package `vike-tradehub`) and `./vike-cli` (`print_version`).
const RELEASE_PRINTERS: &[&str] = &[
    "crates/vike/src/main.rs",
    "crates/vike-tradehub/src/tradehub_cli.rs",
    "crates/vike-cli/src/lib.rs",
];

#[test]
fn every_version_printer_states_its_build_identity() {
    let bare: Vec<String> = version_printers()
        .into_iter()
        .filter(|(_, uses_identity)| !uses_identity)
        .map(|(file, _)| file)
        .filter(|file| !WITHOUT_IDENTITY.iter().any(|(f, _)| f == file))
        .collect();

    assert!(
        bare.is_empty(),
        "these binaries answer `--version` with a bare name+version and cannot say which commit \
         they were built from:\n{}\n\n\
         A release binary was once built from a bare repo four commits behind `main` and nearly \
         installed on the live recorder; `crates/vike-buildinfo/src/lib.rs` carries that incident. \
         Two responses, in order of preference:\n  \
         1. print `vike_buildinfo::version_line(env!(\"CARGO_PKG_NAME\"), env!(\"CARGO_PKG_VERSION\"))` \
         — the name and version stay the first two tokens, so nothing that reads them positionally \
         breaks;\n  \
         2. add a row to WITHOUT_IDENTITY in this file, with the reason it cannot.",
        bare.join("\n  ")
    );
}

/// The NAME a `--version` line carries is the printing package's OWN: every `version_line(` call a
/// printer makes passes `env!("CARGO_PKG_NAME")` as its first argument, spelled at that call.
///
/// ⚠ `scripts/assert_release_identity.sh` relies on this at TAG time. It refuses a bare `./NAME`
/// whose `--version` prints any name but `NAME`, and `crates/vike-ops/tests/release/release_identity_gate.rs`
/// holds each asserted `--bin NAME` to `-p NAME` — the bin half. This is the printer half. Without
/// it a literal (`version_line("vike", …)`), a shared helper's parameter or a constant would pass
/// every PR and be refused only after the tag exists.
///
/// ⚠ What runs this on a PR that touches only a printer's crate is NOT the roster test lane (it
/// selects the touched crate, not this one): it is `.github/workflows/ci.yml`'s `plan` job step
/// "vike-buildinfo's real git probe", `cargo test --release -p vike-buildinfo`, which runs on every
/// PR and push. Moving, narrowing or renaming that step silently removes this test's PR-time effect.
#[test]
fn every_version_printer_prints_its_own_package_name() {
    let mut checked: Vec<String> = Vec::new();
    let mut wrong: Vec<String> = Vec::new();
    for (file, calls) in version_printer_calls() {
        for name in calls.iter().flat_map(|args| version_line_names(args)) {
            if name != OWN_PACKAGE_NAME {
                wrong.push(format!("  {file}: version_line({name}, …)"));
            }
            checked.push(file.clone());
        }
    }
    assert!(
        wrong.is_empty(),
        "these `--version` printers render a name other than their own package's:\n{}\n\n\
         Write `vike_buildinfo::version_line(env!(\"CARGO_PKG_NAME\"), env!(\"CARGO_PKG_VERSION\"))` \
         AT the printing call, in the binary's own package. scripts/assert_release_identity.sh \
         refuses a release binary whose `--version` names another binary, and it finds out at tag \
         time.",
        wrong.join("\n")
    );
    for printer in RELEASE_PRINTERS {
        assert!(
            checked.iter().any(|f| f == printer),
            "{printer} is a printer the release's identity call reaches, and this test checked no \
             `version_line` name in it (checked: {checked:?}) — it moved, or the walker went blind"
        );
    }
}

#[test]
fn no_stale_exemptions() {
    let root = repo_root();
    let printers = version_printers();
    let mut stale = Vec::new();
    for (file, why) in WITHOUT_IDENTITY {
        if !root.join(file).exists() {
            stale.push(format!("  {file} — no such file; delete the row ({why})"));
            continue;
        }
        match printers.iter().find(|(f, _)| f == file) {
            None => {
                stale.push(format!("  {file} — no longer prints a --version line; delete the row"))
            }
            Some((_, true)) => {
                stale.push(format!("  {file} — now uses version_line; delete the row"))
            }
            Some((_, false)) => {}
        }
    }
    assert!(
        stale.is_empty(),
        "stale WITHOUT_IDENTITY rows — an exemption that has stopped being real is a lie the next \
         reader believes:\n{}",
        stale.join("\n")
    );
}

/// A floor, not a count. This gate is textual, and a walker that quietly stopped matching anything
/// would pass both assertions above by seeing an empty tree. The number is below today's and exists
/// only to make "sees nothing" fail loudly — the first cut of this matcher was line-based, went
/// blind the moment `rustfmt` wrapped a call across four lines, and reported 3 printers where there
/// are 6. That is precisely the failure this floor catches.
#[test]
fn the_gate_actually_sees_the_tree() {
    let printers = version_printers();
    assert!(
        printers.len() >= 5,
        "only {} --version printers found — the walker or the matcher is broken: {printers:?}",
        printers.len()
    );
    assert!(
        printers.iter().filter(|(_, uses_identity)| *uses_identity).count() >= 3,
        "no printer states its identity — `version_line` detection is broken: {printers:?}"
    );
    assert!(
        printers.iter().any(|(f, _)| f == "crates/vike-cli/src/lib.rs"),
        "vike-cli's dispatcher must be seen as a --version printer: {printers:?}"
    );
}
