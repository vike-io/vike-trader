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
//! Two shapes, both text-only under some `crates/**/src/**.rs` with comments stripped — the same
//! mechanism `crates/vike-ops/tests/settings_secrets/settings_registry.rs` uses, and the only kind
//! of gate that has ever held in this repo.
//!
//!   1. **A `Program::new(<bin>, <version>)` call.** Every program that reads its command line with
//!      `clap` prints `-V`/`--version` through `vike_util::cli::Program` (decision 0155), and
//!      `CliExit::emit` prints the `<version>` argument VERBATIM — so that argument IS the
//!      `--version` answer, and there is no `println!` left in the program to see. The argument is
//!      followed to where the line is built: through the `let` that binds it (the nearest earlier
//!      one in the file), through a function it calls (this file's first, then the crate's other
//!      files — a bin's `binutil::bin_version_line` lives in a sibling), and through what that binds
//!      or calls in turn ([`scan::program_version_texts`], four steps). The call is a printer when
//!      the text it resolves to names the package version or calls `version_line`. A `Program::new`
//!      given `""` (the `--help` rendering call) is not one.
//!   2. **A `println!` whose own ARGUMENTS name the package version.** The dispatcher's
//!      (`crates/vike/src/main.rs`'s `Route::Version`) is the one left.
//!
//! It deliberately does NOT match every mention of `CARGO_PKG_VERSION`:
//! `crates/vike-cli/src/cmd/mcp.rs`'s `SERVER_VERSION` is an MCP protocol field and
//! `crates/vike-mount/src/incident/manifest.rs`'s `pkg_version` is a report field; neither is a
//! `--version` answer and neither should be forced to carry one.
//!
//! "Names the package version" resolves ONE indirection, crate-wide, for the `println!` shape: a
//! `const`/`static` bound to `env!("CARGO_PKG_VERSION")` counts as the version wherever that name is
//! printed ([`version_aliases`]). A `--version` arm written `const V: &str =
//! env!("CARGO_PKG_VERSION"); println!("{NAME} {V}")` is otherwise INVISIBLE to a literal-only
//! sweep, and the printer floor cannot notice one missing printer. This is the same indirection
//! `settings_registry.rs` resolves for its `const *_ENV: &str` rows and for the same reason —
//! crate-wide rather than file-local, because the constant routinely lives in a sibling file.
//!
//! # Declared blind spots
//!
//! Text only, and a bounded number of steps. A `Program::new` whose version argument is a function
//! PARAMETER (a helper `fn program(version: &str) -> Program`, whose caller holds the real line), a
//! value read out of a struct or a static, or a function more than four steps from the
//! `version_line`/`CARGO_PKG_VERSION` text; a version assembled by `concat!`/`format!` into a name
//! this sweep never sees; a literal line (`Program::new("tool", "tool 1.2")`); or a macro of our own
//! would still slip through. That is stated rather than tolerated silently: closing it needs a real
//! resolver, and the honest position is that this gate catches the shapes a `--version` arm is
//! actually written in today. `crates/vike-ops/tests/settings_secrets/settings_registry.rs` takes
//! the same line — a blind spot has to be declared to exist. A helper that takes the line as a
//! parameter is the shape the migration's groups used (`vike-cli`'s `cmd::args::program`,
//! `vike-backtesthub`'s `backtest_cli::args::program`) and the resolver reads it where the line is BUILT
//! (the function named like the parameter, or the caller's `let`), not at the parameter.
//!
//! The NAME check ([`every_version_printer_prints_its_own_package_name`]) reads the printing CALL,
//! not who calls it, so it cannot see:
//!
//!   * a printer in a LIBRARY crate that another package's binary calls. It spells
//!     `env!("CARGO_PKG_NAME")` correctly and passes here, yet expands to the LIBRARY's name — a
//!     `--version` moved into a shared helper crate is exactly that shape. `vike_util::cli` is
//!     deliberately not that: it takes the whole line from its caller and builds none;
//!   * which binary a printer belongs to at all. That `./vike-backend trade` reaches
//!     `crates/vike-tradehub/src/tradehub_cli/args.rs` (package `vike-tradehub`) is read from
//!     `crates/vike/src/main.rs`'s `TOOLS`, not derived; `RELEASE_PRINTERS` only pins that the three
//!     printers the release reaches are among those checked;
//!   * a printer this module does not RECOGNISE as one — a `print!`, a `writeln!`, a macro of our
//!     own, a `Program::new` in one of the blind spots above. Outside `RELEASE_PRINTERS` such a
//!     printer FAILS OPEN: a wrong name in it passes. Inside them it fails closed, because the pin
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
///
/// A row names the FILE that holds the printer: the `Program::new` call (see the module doc), or the
/// `println!` of the old shape. A file with several bins' worth of calls (`vike-backfill`'s one
/// `cli.rs` serves five bins) is one row.
const WITHOUT_IDENTITY: &[(&str, &str)] = &[
    (
        "crates/vike-backtesthub/src/backtest_cli/args.rs",
        "the standalone `backtesthub` bin prints no build identity: `backtest_cli::run` takes the stamp \
         as a PARAMETER (`vike_runs::BuildStamp`) and `vike-backend backtesthub`, which depends on this \
         crate, fills it. Wiring vike-buildinfo into vike-backtesthub itself would make every commit \
         rebuild it, because this crate's build script reruns whenever HEAD moves. The row dates \
         from when the file lived in `vike-backtest`, a LIBRARY far below the top of the graph; \
         vike-backtesthub sits at the top (65, under only the multicall), so the cost argument is weaker \
         now, and wiring it is a behaviour change that the move which put it here did not make. \
         (It moved from `backtest_cli.rs` to this file with decision 0155, which put the `-V` line \
         in `Program::new`.)",
    ),
    ("crates/vike-backtesthub/src/bin/cheap_np_run.rs", COMPUTE_RESEARCH_BIN),
    ("crates/vike-backtesthub/src/bin/cheap_np_depth.rs", COMPUTE_RESEARCH_BIN),
    ("crates/vike-backtesthub/src/bin/cheap_np_askgate/main.rs", COMPUTE_RESEARCH_BIN),
    ("crates/vike-backtesthub/src/bin/sport_taker_run.rs", COMPUTE_RESEARCH_BIN),
    (
        "crates/vike-backfill/src/cli.rs",
        "the five `*_backfill` bins (`databento`, `tardis`, `vikedata`, `vike_archive`, `events_api`) \
         are batch tools run by hand, not daemons installed on a trading box — the incident class \
         the build identity exists for — and vike-backfill is a layer-30 LIBRARY that vike-datahub \
         links through its `backfill-serve` feature: a normal dependency on vike-buildinfo would \
         rerun its build script, and rebuild the crate and everything above it, whenever HEAD moves \
         (`crates/vike-buildinfo/CLAUDE.md`, trap 5). Their `-V` is `<bin> <crate version>`: the \
         NAME is the bin's, because `CARGO_PKG_NAME` is `vike-backfill` for every one of them.",
    ),
    ("crates/vike-core/src/bin/replay_journal.rs", LIBRARY_TOOL_BIN),
    ("crates/vike-core/src/bin/vike_stat.rs", LIBRARY_TOOL_BIN),
    ("crates/vike-docs/src/bin/docs_data.rs", LIBRARY_TOOL_BIN),
    ("crates/vike-mount/src/bin/incident.rs", LIBRARY_TOOL_BIN),
    ("crates/bridges/ctrader/src/bin/ctrader_authorize.rs", LIBRARY_TOOL_BIN),
    ("crates/bridges/hyperliquid/src/bin/hyperliquid_builder_fee_approve.rs", LIBRARY_TOOL_BIN),
    (
        "crates/vike-studio-core/src/study_cli.rs",
        "`vike-study` is a bin of the layer-35 LIBRARY `vike-studio-core` (its `study-cli` feature, off \
         by default) that `vike-backend study` reaches through the dispatcher; its `-V` is \
         `vike-study <crate version>`. The dispatcher's own `--version`, which the release asserts, \
         does state the identity. A normal dependency on vike-buildinfo would rerun the build \
         script, and rebuild the crate and everything above it, whenever HEAD moves \
         (`crates/vike-buildinfo/CLAUDE.md`, trap 5).",
    ),
];

/// The reason shared by the research bins of `vike-backtesthub`: its `backtest` row says why the crate
/// cannot name vike-buildinfo, and these bins print `vike_backtesthub::binutil::bin_version_line`.
const COMPUTE_RESEARCH_BIN: &str = "a research/measurement bin of `vike-backtesthub`, whose `-V` is \
     `vike_backtesthub::binutil::bin_version_line`: `<bin> <package version>`. The `backtest` row above \
     says why this crate does not name vike-buildinfo (its build script reruns whenever HEAD \
     moves, and the crate sits under only the multicall). None of these is among the three targets \
     the release's identity step asserts, and each is run by hand over recorded data, never \
     installed as a daemon.";

/// The reason shared by the tool bins that live in a mid-graph LIBRARY crate (`vike-core`,
/// `vike-docs`, `vike-mount`, `vike-ctrader`, `vike-hyperliquid`): their `-V` is `<bin> <package
/// version>`.
const LIBRARY_TOOL_BIN: &str = "a tool bin of a LIBRARY crate: run by hand (`docs_data`, by the \
     release's asset step), never a daemon, and not among the three targets the release's identity \
     step asserts. Its `-V` is `<bin> <package version>`. `crates/vike-buildinfo/CLAUDE.md`'s trap 5 \
     is the cost: wired means top-of-graph, because the build script reruns whenever HEAD moves and \
     a normal dependency rebuilds that crate and everything above it — for `vike-core`, the fold \
     every binary links.";

/// The printers `.github/workflows/release.yml`'s identity call reaches: `./vike-backend` (the
/// dispatcher's own `Route::Version`, a `println!`), `"./vike-backend trade"` (the `trade` row's
/// `vike_tradehub::tradehub_cli::run`, package `vike-tradehub`, whose `read_args` hands
/// `Program::new` the version line) and `./vike-cli` (`cmd::args::program`, which `args::parse`
/// builds the root command's `-V` from and every verb's too).
const RELEASE_PRINTERS: &[&str] = &[
    "crates/vike/src/main.rs",
    "crates/vike-tradehub/src/tradehub_cli/args.rs",
    "crates/vike-cli/src/cmd/args.rs",
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
         AT the printing call (for a `Program::new`, in the `let` or function that builds the line it \
         is given), in the binary's own package. scripts/assert_release_identity.sh \
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
/// would pass both assertions above by seeing an empty tree. The numbers are below today's and exist
/// only to make "sees nothing" fail loudly — the first cut of this matcher was line-based, went
/// blind the moment `rustfmt` wrapped a call across four lines, and reported 3 printers where there
/// are 6. That is precisely the failure this floor catches.
///
/// Today's tree (decision 0155, every program's `-V` printed by `Program::new` or the
/// dispatcher's `println!`): the files that state their identity are `vike`'s
/// `main.rs` and `tearsheet_cli.rs`, `vike-cli`'s `cmd/args.rs`, `vike-datahub`'s `datahub_cli/args.rs`,
/// `vike-strategy-builder`'s `builder.rs` and `vike-tradehub`'s `tradehub_cli/args.rs`, `catalog_cli.rs`,
/// `notify_cli.rs` and `venues_cli.rs` (nine), and the rest are the [`WITHOUT_IDENTITY`] rows (thirteen,
/// each checked to be a real bare printer by `no_stale_exemptions`). The floors
/// leave room for a printer to be merged or retired without a gate edit, but not for a whole SHAPE
/// to go unseen: the bare printers are all `Program::new` ones, so a resolver that stopped following
/// a `let` or a function takes the bare floor (and the total) below it.
#[test]
fn the_gate_actually_sees_the_tree() {
    let printers = version_printers();
    assert!(
        printers.len() >= 15,
        "only {} --version printers found — the walker or the matcher is broken: {printers:?}",
        printers.len()
    );
    assert!(
        printers.iter().filter(|(_, uses_identity)| *uses_identity).count() >= 6,
        "too few printers state their identity — `version_line` detection (or the \
         `Program::new` resolver) is broken: {printers:?}"
    );
    assert!(
        printers.iter().filter(|(_, uses_identity)| !uses_identity).count() >= 8,
        "too few bare printers — the `Program::new` shape (or its `let`/function resolver) went \
         blind: {printers:?}"
    );
    assert!(
        printers.iter().any(|(f, _)| f == "crates/vike-cli/src/cmd/args.rs"),
        "vike-cli's `Program::new` must be seen as a --version printer: {printers:?}"
    );
}
