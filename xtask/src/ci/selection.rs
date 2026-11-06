//! What a changed file selects beyond its owning crates: gate crates, lane readers, suites, escalation.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::git;
use super::{lockfile, set, tables};

/// The gate-owning crates a change must select REGARDLESS of which crate owns the file.
///
/// Every entry reads an input OUTSIDE its own crate's sources — the whole `.rs` tree, the markdown
/// tree, `deploy/`, a repo-root manifest, every crate's `Cargo.toml` — so the reverse-dep closure,
/// derived from changed CRATES, structurally cannot select them. A gate that cannot run on its own change class is not a gate:
/// measured, a docs-only PR selected zero crates and not one of the five prose gates ran, and a
/// `server.json`-only PR selected zero crates and did not run the manifest's drift gate.
pub fn gate_crates_for(files: &[String]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    if files.iter().any(|f| f.ends_with(".rs")) {
        out.extend(tables::gate_crates::SETTINGS_GATE_CRATES.iter().map(|c| (*c).to_string()));
    }
    let doc_input = |f: &String| {
        tables::gate_crates::DOC_GATE_INPUT_SUFFIXES.iter().any(|s| f.ends_with(s))
            || tables::gate_crates::DOC_GATE_INPUT_PREFIXES.iter().any(|p| f.starts_with(p))
    };
    if files.iter().any(doc_input) {
        out.extend(tables::gate_crates::DOC_GATE_CRATES.iter().map(|c| (*c).to_string()));
    }
    // A crate's OWN manifest (the root one has no `/` and escalates on its own) — see
    // `tables::gate_crates::CRATE_MANIFEST_GATE_CRATES` for the gates that read every one.
    if files.iter().any(|f| f.ends_with("/Cargo.toml")) {
        out.extend(
            tables::gate_crates::CRATE_MANIFEST_GATE_CRATES.iter().map(|c| (*c).to_string()),
        );
    }
    // Exact paths rather than a prefix — see `tables::gate_crates::MANIFEST_GATE_INPUTS` for why the repository
    // root cannot be swept wholesale.
    if files.iter().any(|f| tables::gate_crates::MANIFEST_GATE_INPUTS.contains(&f.as_str())) {
        out.insert(tables::gate_crates::MANIFEST_GATE_CRATE.to_string());
    }
    // Exact paths again — the files the docs-data gate reads; see `tables::gate_crates::DOCS_DATA_GATE_INPUTS`.
    if files.iter().any(|f| tables::gate_crates::DOCS_DATA_GATE_INPUTS.contains(&f.as_str())) {
        out.insert(tables::gate_crates::DOCS_DATA_GATE_CRATE.to_string());
    }
    out
}

/// The crates a changed file must put in the test LANE because their tests READ it, though it
/// belongs to no crate — never `affected`, so no feature suite fires for them.
///
/// Two sources: any path [`tables::escalation::is_global_exempt`] answers for selects
/// [`tables::readers::EXEMPT_INPUT_GATE_CRATES`] (whose gates read or run all of them), and a path matching a
/// [`tables::readers::LANE_INPUT_READERS`] row selects that row's crates. Keyed on the FILES, like
/// [`gate_crates_for`]; it differs from that function only in where its answer goes, and the
/// difference is the point — `gate_crates_for` feeds `affected` (so its crates fire the suites
/// they trigger), this feeds only `ordered`.
///
/// It does not ask whether a path ESCALATES: an escalating change plans the whole roster anyway,
/// so a lane addition there is a no-op rather than an error.
pub fn lane_crates_for(files: &[String]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    if files.iter().any(|f| tables::escalation::is_global_exempt(f)) {
        out.extend(tables::readers::EXEMPT_INPUT_GATE_CRATES.iter().map(|c| (*c).to_string()));
    }
    for (prefix, suffix, readers) in tables::readers::LANE_INPUT_READERS {
        if files.iter().any(|f| f.starts_with(prefix) && f.ends_with(suffix)) {
            out.extend(readers.iter().map(|r| (*r).to_string()));
        }
    }
    out
}

/// True iff the `datahub-all` job should run — the DATA side's one feature build
/// (`docs/decisions/0132-the-feature-suites-fold-into-one-data-job-and-one-trading-job.md`, amending
/// `docs/decisions/0131-the-datahub-is-verified-by-its-default-build-and-one-all-features-build.md`).
/// Three doors, each the narrowest that still sees every edit the build can observe:
///   * a crate of [`tables::roster::DATAHUB_ALL_CRATES`] is in `direct`, the crates that OWN a changed
///     file (every crate when a global file changed, and the members that link a moved
///     `[workspace.dependencies]` package): the job runs or lints those crates' test targets, so a
///     test-only edit of one counts. It is NOT the reverse-dependency closure: a source edit in a crate
///     BELOW them (vike-model above all) is not an edit of theirs, and the closure fired a ~208-second
///     DataFusion compile on it (MEASURED over the 30 merged PRs before the `hist-datafusion` job took
///     this rule: 25 fired it and 16 owned a hist crate's file);
///   * a crate of [`tables::roster::DATAHUB_ALL_DEPENDENCY_CRATES`] is in `compiled`, the crates with an
///     edit something can LINK (`plan::compute`'s set of that name: no test-target source, no
///     `cfg(test)` module, a `test-support` module counted). The job compiles those crates as
///     dependencies only, so a test-only edit of one cannot move it;
///   * a changed file is a [`tables::readers::DATAHUB_ALL_JOB_INPUTS`] entry — owned by no crate, read
///     by a test only this build compiles.
///
/// It is the union of what fired the jobs and suites it replaced: the four vike-datahub feature suites
/// (decision 0131), the `hist-datafusion` job (vike-data and vike-backfill, direct; vike-store, the
/// crate its DataFusion backend became, joins them) and the `backtest-hist-replay` suite
/// (vike-backtest, direct; vike-backtesthub joined it when the daemon and the bins moved). So a change that fired one of those builds fires this one.
///
/// ⚠ **The accepted risk, stated (owner-approved 2026-10-11 for the `hist-datafusion` job, "ok"):** a
/// semantic change in a LOWER crate that breaks a test only this job runs (vike-datahub's
/// feature-gated tests, vike-backfill's) is seen only on the `push` run after the merge. The
/// DataFusion store is outside that gap now: its Parquet codec over vike-model's own structs (matching
/// `BookUpdateKind` exhaustively, reading `SymbolProperties` fields) is the roster crate vike-store,
/// whose tests the `test` job runs over the reverse-dependency closure. What still catches the COMPILE
/// half before the merge: [`tradehub_affected`]'s job, whose `-p vike-backend --features full` builds
/// the datahub's shipped features and fires on the closure. Reopen when a pull request merges green
/// and `datahub-all` is red on the `push` run that follows, from an edit outside these tables.
pub fn datahub_affected(
    direct: &BTreeSet<String>,
    compiled: &BTreeSet<String>,
    files: &[String],
) -> bool {
    direct.intersection(&set(tables::roster::DATAHUB_ALL_CRATES)).next().is_some()
        || compiled
            .intersection(&set(tables::roster::DATAHUB_ALL_DEPENDENCY_CRATES))
            .next()
            .is_some()
        || files.iter().any(|f| tables::readers::DATAHUB_ALL_JOB_INPUTS.contains(&f.as_str()))
}

/// True iff the `tradehub-all` job should run — the TRADING side's one feature build
/// (`docs/decisions/0132-the-feature-suites-fold-into-one-data-job-and-one-trading-job.md`). Two doors:
///   * a crate of [`tables::roster::TRADEHUB_ALL_CRATES`] is in `direct`: the job runs those crates'
///     tests, so a test-only edit of one counts (the deleted `ibkr`, `fxcm`, `polymarket-stack`,
///     `telegram` and `polymarket` suites' direct rule, and `multicall`'s test-built row for
///     vike-backend);
///   * a crate of [`tables::roster::TRADEHUB_ALL_CLOSURE_CRATES`] is in `closure`, the reverse-dependency
///     closure of every edit something can BUILD (`plan::compute`'s set of that name): the job compiles
///     the whole stack from its top, so a source edit far below a trigger moves it. That is the rule the
///     deleted `multicall` and `workspace-bins` suites had (decision 0104 kept the walk for them alone),
///     and a test-only edit of a closure trigger fires nothing, as it fired neither of them
///     (decision 0123, item 3).
///
/// The union of what fired the seven suites it replaced, so a change that fired one of them fires this
/// job. What it gives up, by the owner's ruling, is the proof that one feature compiles ALONE (the
/// tradehub's `telegram`, `polymarket`, `ibkr` or `fxcm` without the others, vike-polymarket's
/// `polymarket` without the rest of the stack), which `.github/workflows/feature-matrix.yml`'s
/// `cargo hack --each-feature` keeps on demand.
pub fn tradehub_affected(direct: &BTreeSet<String>, closure: &BTreeSet<String>) -> bool {
    direct.intersection(&set(tables::roster::TRADEHUB_ALL_CRATES)).next().is_some()
        || closure.intersection(&set(tables::roster::TRADEHUB_ALL_CLOSURE_CRATES)).next().is_some()
}

/// True when a change to `f` invalidates the narrow selection and plans the FULL roster.
///
/// [`tables::escalation::GLOBAL_PREFIXES`] (and the root `Cargo.toml`), minus what [`tables::escalation::is_global_exempt`]
/// exempts — and an exempted path is exempt only while it still EXISTS under `cwd`. A deleted or
/// renamed-away file is the one change to an exempted input that can break a test no
/// [`tables::readers::LANE_INPUT_READERS`] row selects: every crate's citations and existence checks name
/// these paths, and an edit cannot move an existence check while a deletion always does. That
/// rule needs the OLD path of a rename in the file list, which is why [`git::diff_names`] passes
/// `--no-renames`.
///
/// ⚠ The exemption is tested FIRST, and it is a per-FILE decision rather than a per-commit one: a
/// change touching `scripts/qa_shots.sh` AND `scripts/ci_feature_suite.sh` still escalates,
/// because the second is global on its own.
///
/// ⚠ A PATH question, and for two files not the last word: `ci.yml` and the feature-suite script
/// ([`tables::escalation::NARROWED_GLOBAL_FILES`]) answer `true` here and are then judged by the part
/// their edit moved (`super::global_parts`, decision 0142), which leaves them global only on a doubt.
///
/// ⚠ Those two examples are REAL files on purpose. `crates/vike-ops/tests/docs/citation_gate.rs` reads a
/// BACKTICKED path as a citation and requires it to resolve, so an invented placeholder name under
/// a real directory reddens CI — which is exactly what an earlier draft of the comment this
/// replaced did. An illustrative path must either name a file that exists or carry no backticks.
pub fn escalates(f: &str, cwd: &Path) -> bool {
    if tables::escalation::is_global_exempt(f) && cwd.join(f).exists() {
        return false;
    }
    f == "Cargo.toml" || tables::escalation::GLOBAL_PREFIXES.iter().any(|p| f.starts_with(p))
}

/// True when the only change `base..HEAD` made to the global file `f` is whole-line `#` comments
/// and blank lines — so it is NOT a global edit: it neither escalates ([`escalates`]) nor fires the
/// latency gate ([`latency_affected`]).
///
/// Asked only of a file that would otherwise be global (it escalates, or it is a latency input) and
/// whose comment syntax is `#` ([`tables::escalation::has_hash_comments`]); the diff is read by
/// [`git::comment_only_change`], and every doubt there is `false`, so this answers `true` only on
/// positive evidence.
///
/// ⚠ **The edit is still a CHANGE to a file some gate reads.** A comment in the root manifest, in
/// `ci.yml` or in `deny.toml` is still text that `crates/vike-ops`'s manifest, config and workflow
/// gates read and parse, so the caller adds [`tables::readers::EXEMPT_INPUT_GATE_CRATES`] to the
/// lane for it, exactly as for an exempted file — and nothing else: no full roster, no feature
/// suite, no latency measurement.
pub fn comment_only_global(f: &str, base: &str, cwd: &Path) -> bool {
    if !((escalates(f, cwd) || is_latency_input(f)) && tables::escalation::has_hash_comments(f)) {
        return false;
    }
    // A YAML file (`ci.yml` and the composite actions) is judged by the whole-text rule of decision
    // 0148, which sees a block scalar; every other `#` file keeps the per-line diff reader.
    if tables::escalation::is_yaml(f) {
        git::yaml_comment_only_change(base, f, cwd)
    } else {
        git::comment_only_change(base, f, cwd)
    }
}

/// True iff `f` is a non-crate file that fires the latency gate whatever its change:
/// [`tables::escalation::LATENCY_GLOBAL_PREFIXES`] (the toolchain pin, `.cargo/`, the verdict script).
fn is_latency_input(f: &str) -> bool {
    tables::escalation::LATENCY_GLOBAL_PREFIXES.iter().any(|p| f.starts_with(p))
}

/// The workspace crates that compile into the binary the the latency box latency gate measures: the four
/// [`tables::roster::LATENCY_CRATES`] and every workspace member in their NORMAL (+build) closure
/// (`normal_closure`, [`super::graph::Graph::normal_closure`], met with `members`). DERIVED, never
/// listed: a crate inserted below any of the four joins it with no edit here. On 2026-10-11's tree it
/// is six crates, the four plus `vike-journal` (under vike-core) and `vike-marketdata` (under
/// vike-data). A measured crate the graph does not hold contributes its own name only; the planner
/// refuses such a graph outright (`plan::compute`'s graph check).
pub fn measured_crates(
    normal_closure: &BTreeMap<String, BTreeSet<String>>,
    members: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut out = set(tables::roster::LATENCY_CRATES);
    for &krate in tables::roster::LATENCY_CRATES {
        let below = normal_closure.get(krate).into_iter().flatten();
        out.extend(below.filter(|c| members.contains(*c)).cloned());
    }
    out
}

/// True iff this change can move the binary the the latency box latency gate measures — the `core` output,
/// and ONE rule for every event: a pull request, a push to main and the merge queue ask the same
/// question (decision 0130; until then a pull request also needed a narrower `core_direct`, and the
/// push rule fired on the root manifest and on `ci.yml`). The gate fires when the change
///
///   * edits a file of a [`measured_crates`] member that compiles into the binary (`changed_crates`,
///     the planner's `lat_changed`: [`latency_compiled`] drops test-only files and crate documents,
///     the harness's own files excepted, and a `.rs` file whose edit changes only comments);
///   * edits a [`tables::escalation::LATENCY_GLOBAL_PREFIXES`] file (the compiler, its flags, the
///     verdict script);
///   * edits `Cargo.lock` so that a package entry of the measured crates' closure changes, which
///     `lock_moves_latency` answers ([`latency_lock_moved`]; asked only when the lockfile is in
///     `files`, and the planner fails safe on every doubt);
///   * edits [`tables::escalation::LATENCY_JOB_WORKFLOW`] so that the text of the gate's own job
///     block changes (its steps, thresholds, pinning, quiesce logic; comments inside the block
///     count), which `job_moves_latency` answers ([`latency_job_moved`]; asked only when that
///     workflow is in `files`, and it fails safe too). An edit of another job does not fire;
///   * or edits the root `Cargo.toml` so that the text of its `[profile.*]` tables changes (how the
///     binary is compiled), which `profile_moves_latency` answers ([`latency_profile_moved`]; asked
///     only when the root manifest is in `files`, fail safe). Any other root-manifest edit does not
///     fire by itself: a version it moves reaches the gate through the lock.
///
/// ⚠ Deliberately NOT `"vike-core" in affected`, which is what it was before 2026-09. `affected` is
/// widened by one hop of DEV-dependents (vike-core dev-depends upward on vike-mm/vike-strategy/
/// vike-script, none of which is in its compiled library) and by ESCALATION (any workspace-global
/// file made `affected` every crate, so `deny.toml`, the `justfile` or `.github/CODEOWNERS` each
/// bought an ~11-minute microsecond measurement on reserved cores).
///
/// ⚠ **What it does not see, by the owner's ruling:** a feature-only `[workspace.dependencies]` edit
/// of the root `Cargo.toml` moves no lock entry and no profile, and fires nothing. Nothing measures
/// such an edit until a later change fires the gate; decision 0130 states the risk.
pub fn latency_affected(
    files: &[String],
    changed_crates: &BTreeSet<String>,
    measured: &BTreeSet<String>,
    lock_moves_latency: impl FnOnce() -> bool,
    job_moves_latency: impl FnOnce() -> bool,
    profile_moves_latency: impl FnOnce() -> bool,
) -> bool {
    let has = |name: &str| files.iter().any(|f| f == name);
    files.iter().any(|f| is_latency_input(f))
        || changed_crates.iter().any(|c| measured.contains(c))
        || (has("Cargo.lock") && lock_moves_latency())
        || (has(tables::escalation::LATENCY_JOB_WORKFLOW) && job_moves_latency())
        || (has("Cargo.toml") && profile_moves_latency())
}

/// The text of the latency gate's job block in a workflow ([`job_block`] for
/// [`tables::escalation::LATENCY_JOB_KEY`]).
pub fn latency_job_block(workflow: &str) -> Option<String> {
    job_block(workflow, tables::escalation::LATENCY_JOB_KEY)
}

/// True iff an edit of [`tables::escalation::LATENCY_JOB_WORKFLOW`] changed the text of the gate's
/// own job block ([`job_moved`] for [`tables::escalation::LATENCY_JOB_KEY`]).
pub fn latency_job_moved(old: Option<&str>, new: Option<&str>) -> bool {
    job_moved(old, new, tables::escalation::LATENCY_JOB_KEY)
}

/// The text of job `key`'s block in a workflow: from the line `  <key>:` up to the next non-blank
/// line indented two spaces or less (the next job's key, or the comment that opens it), trailing
/// blank lines dropped. `None` when the workflow has no such line. The comment ABOVE the key is the
/// previous gap's, not the block's: it describes the job and moves nothing it runs. Read for the
/// latency gate ([`latency_job_block`]), the API-reference job
/// ([`tables::readers::DOCS_JOB_KEY`], decision 0137), the LightGBM job
/// ([`tables::readers::LIGHTGBM_JOB_KEY`], decision 0138), and every job of `ci.yml` through
/// `super::global_parts::workflow_jobs` (decision 0142).
pub fn job_block(workflow: &str, key: &str) -> Option<String> {
    let header = format!("  {key}:");
    let mut lines = workflow.lines();
    lines.by_ref().find(|l| l.trim_end() == header)?;
    let mut block = header;
    for line in lines {
        let indent = line.len() - line.trim_start_matches(' ').len();
        if !line.trim().is_empty() && indent <= 2 {
            break;
        }
        block.push('\n');
        block.push_str(line);
    }
    Some(block.trim_end().to_string())
}

/// True iff the text of job `key`'s block ([`job_block`]) differs between `old` (the workflow at the
/// diff base) and `new` (at HEAD), both from git, in anything but comments and blank lines: the blocks
/// are compared by [`super::yaml_comments::comment_only`] (decision 0148), so a whole-line comment
/// inside the block moves nothing unless it sits in a `run: |` script that could make it data.
/// **FAIL SAFE:** no text on either side (a new, deleted or unreadable workflow) or no block in either
/// text (the job renamed or moved) is `true`.
pub fn job_moved(old: Option<&str>, new: Option<&str>, key: &str) -> bool {
    let (Some(old), Some(new)) = (old, new) else { return true };
    match (job_block(old, key), job_block(new, key)) {
        (Some(a), Some(b)) => !super::yaml_comments::comment_only(&a, &b),
        _ => true,
    }
}

/// The text of every `[profile.*]` table of a root `Cargo.toml`, in file order: each table's header
/// line and every line up to the next table header (comments and blank lines included, so a comment
/// inside a profile table is part of it). These tables decide HOW the measured binary is compiled
/// (opt-level, `lto`, codegen-units, panic, debug), for `[profile.release]`, its
/// `[profile.release.package.*]` overrides, `[profile.dev]`, `[profile.test]` and any other.
///
/// `None` = a line that opens with `[` but is not a well-formed table header (`[a.b]` or `[[a]]`,
/// with an optional trailing `#` comment): the table structure is not known, so the caller fires.
pub fn profile_tables(manifest: &str) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    let mut inside = false;
    for line in manifest.lines() {
        let t = line.trim_start();
        if t.starts_with('[') {
            let header = t.split('#').next().unwrap_or_default().trim_end();
            if !header.ends_with(']') {
                return None;
            }
            let name = header.trim_matches(|c| c == '[' || c == ']').trim();
            inside = name == "profile" || name.starts_with("profile.");
        }
        if inside {
            out.push(line);
        }
    }
    Some(out.join("\n").trim_end().to_string())
}

/// True iff an edit of the root `Cargo.toml` changed the text of its `[profile.*]` tables
/// ([`profile_tables`]) between `old` (the manifest at the diff base) and `new` (at HEAD), both from
/// git. Every other root-manifest edit (members, `[workspace.dependencies]`, `[workspace.package]`,
/// lints, a comment outside those tables) moves the measured binary only through the lock it
/// resolves, which [`latency_lock_moved`] judges. **FAIL SAFE:** no text on either side, or a text
/// whose header region does not read, is `true`.
pub fn latency_profile_moved(old: Option<&str>, new: Option<&str>) -> bool {
    let (Some(old), Some(new)) = (old, new) else { return true };
    match (profile_tables(old), profile_tables(new)) {
        (Some(a), Some(b)) => a != b,
        _ => true,
    }
}

/// True iff an edit of `file` can reach the binary the the latency box latency gate measures, given whether the
/// planner classified it test-only (`graph::is_test_target_source`, or a `graph::is_cfg_test_module`
/// module): a file that is not test-only always can, and a test-only one cannot unless it is one of
/// the files the `runtime_latency` binary itself compiles, [`tables::roster::LATENCY_HARNESS_FILES`].
///
/// WHY a test-only file cannot. The gate builds `cargo test -p vike-core --release --test
/// runtime_latency`: the LIBRARIES of vike-core and its normal dependencies, plus the harness. A
/// dependent's build never compiles another crate's `tests/`, `benches/` or `examples/`, nor a
/// module behind `#[cfg(test)]` (cfg(test) is on only for that crate's OWN test build), so such an
/// edit leaves every instruction of the measured binary byte-identical. MEASURED 2026-10-11 over the
/// last 30 merged PRs: no PR changes its answer (0 of 30), the rule exists so the next gate-fix PR
/// to a measured crate does not hold the one `the latency runner` runner for 2-8 minutes.
///
/// ⚠ The harness files are test-target sources AND the measurement: a narrower reading would let a
/// change to how the p99 is MEASURED skip the measurement. A `test-support` module
/// (`graph::feature_test_module`) is not test-only here: vike-core's dev edges turn the feature on
/// for vike-exec, vike-model and vike-data in this very binary (`RecordingClient` is one), so the
/// caller passes `false` for it. A data file under `tests/` is not test-only either
/// (`graph::is_test_target_source` is `.rs` only).
///
/// The THIRD drop (decision 0139, amending 0130): a `.rs` file whose edit changes only comments,
/// which `comment_only` answers (`plan::compute` answers it from ONE set for the whole change,
/// [`git::rust_comment_only_files`], at the run's one base; `false` for a build script and every
/// doubt). Equal token streams at the base and at
/// HEAD are the same program for the compiler, so the measured instructions cannot move; the rule and
/// its doubts are [`super::rust_comments`]'s. The same set takes such a file out of the planner's
/// reverse-dependency walk and its jobs, unless another crate reads its text
/// ([`tables::readers::SOURCES_READ_AS_TEXT`]); the latency drop does not ask that. It applies to the
/// harness files too (a comment in the harness
/// moves no measured instruction either), and to no file that is not `.rs` (a manifest, a data file
/// under `tests/`: those keep their rules).
///
/// `plan::compute` seeds [`latency_affected`]'s crate half through it. Every other latency input
/// (`Cargo.lock`, the toolchain pin, `.cargo/`, the verdict script) is a FILE rule this does not touch.
pub fn latency_compiled(file: &str, test_only: bool, comment_only: impl FnOnce() -> bool) -> bool {
    (!test_only || tables::roster::LATENCY_HARNESS_FILES.contains(&file))
        && !(file.ends_with(".rs") && comment_only())
}

/// True iff a `Cargo.lock` change can move the binary the the latency box latency gate measures: some
/// `[[package]]` entry it adds, removes or edits ([`lockfile::changed_lock_entries`]) is one of the
/// [`tables::roster::LATENCY_CRATES`] or in the NORMAL (+build) closure of one of them
/// (`normal_closure`, [`super::graph::Graph::normal_closure`]), third-party packages included. That
/// closure holds every [`measured_crates`] member, so their own entries count too.
///
/// `old` is the lock at the diff base and `new` the lock at HEAD, both from git. **FAIL SAFE: every
/// doubt is `true`.** No base text or no HEAD text (a new, deleted or unreadable lockfile), a text
/// [`lockfile::changed_lock_entries`] cannot compare (it does not parse, or a line outside the entries
/// moved: the format version, a comment, `[[patch.unused]]`), and a measured crate missing from
/// `normal_closure` (renamed, or a graph that did not load it) all fire.
///
/// WHY the lock's entries are enough. A package compiles into the measured binary only through an
/// edge of the closure, and every edge is written in the lock as its parent's `dependencies` item: a
/// version bump changes the moved package's own entry, a new or dropped edge changes the parent's
/// entry, and the parent is in the closure. A package that LEFT the closure was dropped by an edge
/// whose parent is still in it, so that parent's entry changed too: the closure read at HEAD is
/// enough. A changed entry OUTSIDE the closure (`vike-datahub` gaining an edge, `vike-desktop`
/// losing one: the shape of PR #2814) is a package no build of the four links, so it cannot move the
/// measured instructions.
///
/// WHY the NORMAL closure, from `cargo metadata`, and not the lock's own graph. The lock records every
/// edge with no kind, so its closure walks DEV edges too (`vike-core` dev-depends upward on `vike-mm`,
/// `vike-strategy` and `vike-script`, which the crate half of [`latency_affected`] does not count
/// either). Its declared residual: a package reached ONLY through a dev edge of `vike-core`
/// (`tracing-test`, `tempfile`, the `vike-mm` subtree) is compiled into the `runtime_latency` binary
/// and can unify a FEATURE onto a closure package, and a feature is not in the lock; such a change
/// moves no closure entry and fires nothing.
pub fn latency_lock_moved(
    old: Option<&str>,
    new: Option<&str>,
    normal_closure: &BTreeMap<String, BTreeSet<String>>,
) -> bool {
    let (Some(old), Some(new)) = (old, new) else { return true };
    let Some(changed) = lockfile::changed_lock_entries(old, new) else { return true };
    let mut reach: BTreeSet<&str> = BTreeSet::new();
    for &krate in tables::roster::LATENCY_CRATES {
        let Some(closure) = normal_closure.get(krate) else { return true };
        reach.insert(krate);
        reach.extend(closure.iter().map(String::as_str));
    }
    changed.iter().any(|p| reach.contains(p.as_str()))
}

/// Whether a PULL-REQUEST plan leaves `key` out: it is in [`tables::suite_rules::POST_MERGE_SUITES`] and
/// the change moves no dependency that one of the suite's own crates links.
///
/// `moved` is [`super::workspace_deps::moved_consumers`]'s answer (empty when the change moves no
/// package): the members that link a package the root manifest's entries or the lockfile moved. A
/// post-merge suite is KEPT when any of its [`tables::feature_suites::FEATURE_SUITES`] trigger crates is in
/// it — the same members `plan::compute` seeds the direct suites with as `dep_consumers`, so the suite
/// fires for exactly the reason it would on `push`. A source edit, a root edit that moves no
/// package and a global escalation keep nothing: only the EVENT and a moved package decide, never the diff.
/// A key outside `POST_MERGE_SUITES` is never left out here.
pub fn post_merge_dropped(key: &str, moved: &BTreeSet<String>) -> bool {
    tables::suite_rules::POST_MERGE_SUITES.contains(&key)
        && !tables::feature_suites::FEATURE_SUITES
            .iter()
            .filter(|(k, _)| *k == key)
            .any(|(_, triggers)| triggers.iter().any(|t| moved.contains(*t)))
}

/// What `key` costs the packer, in seconds: its [`tables::suite_rules::SUITE_COST_SECONDS`] row, or
/// [`tables::suite_rules::PACK_TARGET_SECONDS`] for a key with no row — which makes the unknown key
/// fill a job by itself, so a missing row can never pack a suite into a neighbour. A pin
/// (`xtask/tests/ci_plan_gate/suite_cost_packing.rs`) holds that no real key is without a row.
pub fn suite_cost_seconds(key: &str) -> u32 {
    tables::suite_rules::SUITE_COST_SECONDS
        .iter()
        .find(|(k, _)| *k == key)
        .map_or(tables::suite_rules::PACK_TARGET_SECONDS, |(_, cost)| *cost)
}

/// Pack the affected suite keys into `features` matrix legs, by COST.
///
/// Input: the affected [`tables::feature_suites::FEATURE_SUITES`] keys, in any order (a repeat is one
/// key). Output: one entry per matrix leg — a key on its own, or several keys joined by single spaces
/// — and the output depends on the SET of keys only, so the leg names are stable run to run.
///
/// The rules, in order, each with its table in [`tables::suite_rules`]:
///   1. a key in [`tables::suite_rules::PACK_ALONE_SUITES`] is its own leg;
///   2. the rest go first-fit-DECREASING by [`suite_cost_seconds`] into jobs whose summed cost stays at
///      most [`tables::suite_rules::PACK_TARGET_SECONDS`] (ties broken by name): each key into the first
///      job it fits, else a new one — so a key whose own cost exceeds the target opens a job nothing
///      else fits into;
///   3. a run of [`tables::suite_rules::PACK_MIN_JOBS`] or more keys never makes fewer jobs than that
///      (the pool has that many `ci-build` runners): while short, the job with the largest summed cost
///      that holds two or more keys gives its costliest key a job of its own. Fewer keys pack freely.
///
/// Inside a leg the keys are sorted by name; the legs are ordered by summed cost, heaviest first (the
/// heaviest start first on a pool that drains six at a time, which shortens the tail), then by name.
/// A lone key is the bare key, so a narrow pull request schedules exactly its suite.
///
/// The space join is the contract with ci.yml's `features` job: `${{ matrix.suite }}` is passed
/// to `scripts/ci_feature_suite.sh` unquoted, the shell word-splits it, and the script runs each
/// argument's `case` arm in order — the same invocations the keys ran as separate jobs. Packing
/// changes WHICH JOB a key runs in, never what it runs.
pub fn pack_suites(keys: &[String]) -> Vec<String> {
    use tables::suite_rules::{PACK_ALONE_SUITES, PACK_MIN_JOBS, PACK_TARGET_SECONDS};

    let mut keys: Vec<&str> = keys.iter().map(String::as_str).collect();
    keys.sort_unstable();
    keys.dedup();
    let total = keys.len();
    let (alone, mut packable): (Vec<&str>, Vec<&str>) =
        keys.into_iter().partition(|k| PACK_ALONE_SUITES.contains(k));
    packable.sort_by(|a, b| suite_cost_seconds(b).cmp(&suite_cost_seconds(a)).then(a.cmp(b)));

    // `(summed cost, members)`; members arrive costliest first, which the floor below relies on.
    let mut jobs: Vec<(u32, Vec<&str>)> = Vec::new();
    for key in packable {
        let cost = suite_cost_seconds(key);
        match jobs.iter_mut().find(|(sum, _)| sum + cost <= PACK_TARGET_SECONDS) {
            Some((sum, members)) => {
                *sum += cost;
                members.push(key);
            }
            None => jobs.push((cost, vec![key])),
        }
    }
    // Split to the floor. Terminates: while `jobs + alone < total` some job holds two keys or more,
    // and every pass adds a job.
    let floor = if total >= PACK_MIN_JOBS { PACK_MIN_JOBS } else { 0 };
    while jobs.len() + alone.len() < floor {
        let split = jobs
            .iter()
            .enumerate()
            .filter(|(_, (_, members))| members.len() > 1)
            .max_by_key(|(i, (sum, _))| (*sum, std::cmp::Reverse(*i)))
            .map(|(i, _)| i)
            .expect("fewer jobs than keys leaves a job holding two keys");
        let key = jobs[split].1.remove(0);
        jobs[split].0 -= suite_cost_seconds(key);
        jobs.push((suite_cost_seconds(key), vec![key]));
    }

    let mut legs: Vec<(u32, String)> = jobs
        .into_iter()
        .map(|(sum, mut members)| {
            members.sort_unstable();
            (sum, members.join(" "))
        })
        .chain(alone.into_iter().map(|k| (suite_cost_seconds(k), k.to_string())))
        .collect();
    legs.sort_by(|(a_cost, a), (b_cost, b)| b_cost.cmp(a_cost).then(a.cmp(b)));
    legs.into_iter().map(|(_, leg)| leg).collect()
}

/// True iff the the CI box LightGBM job should run: a DIRECT edit of a [`tables::roster::LIGHTGBM_CRATES`]
/// crate, an edit of a file in [`tables::readers::LIGHTGBM_JOB_INPUTS`] (the job's own three
/// scripts and the one `vike-model` file its suites compile), or an edit of
/// [`tables::readers::LIGHTGBM_JOB_WORKFLOW`] that changes the text of the job's own `lightgbm:`
/// block, which `job_moves_lightgbm` answers ([`job_moved`] for
/// [`tables::readers::LIGHTGBM_JOB_KEY`]; asked only when that workflow is in `files`, and it fires
/// on every doubt). Another job's edit, a workflow-level key or the comment above the key does not.
///
/// `owners` is the crates that OWN a changed file (a test-only file and a crate document included,
/// as the planner's `changed` set has them) plus the members that link a package the root manifest's
/// `[workspace.dependencies]` or the lockfile MOVED (the planner's `moved`,
/// [`super::workspace_deps::moved_consumers`]) — and NEVER the global expansion: a `ci.yml`,
/// `justfile`, `deny.toml`, `xtask/` or other global-file edit alone, or a root-manifest or lockfile
/// edit that moves no package `vike-ml` links, does not fire the job (decision 0138; until then
/// `owners` was the planner's `direct` set, which is every crate when a global file changed, and a
/// global file fired it on runs that touched nothing it runs). `files` is the raw
/// changed-file list. **FAIL SAFE:** `owners` is `None` when this run has no file list to read (an
/// unresolvable base, `CI_FULL=1`), and the job fires, as the latency gate does.
///
/// ⚠ It was the same shape as the `hist` trigger — an intersection with the AFFECTED set, the
/// reverse-dependency closure plus one dev hop — and that is what this replaced, on this evidence:
/// `vike-ml` has no `vike-*` normal dependency, so nothing below it can change its build, and the
/// three suites `scripts/ci_lightgbm_suite.sh` runs use its dev-dependencies only for
/// `vike_paths::scratch::ScratchDir`, a std-only temp directory they create (`vike-model`'s until the
/// paths left that crate). The closure therefore fired the job (a the CI box job with a real trainer
/// binary) on every `vike-model` edit, 14 of the last 30 pull requests, for a dependency that one
/// file carries; it is 3 of 30 reading the direct set. That file is a row of
/// [`tables::readers::LIGHTGBM_JOB_INPUTS`], and `xtask/tests/ci_plan_gate.rs` holds the claim: the
/// suites name `vike_paths::scratch::`, nothing else of `vike-paths` and nothing of `vike-model`.
///
/// Not [`latency_affected`]'s question either: this job asks "did a file the trainer suites compile
/// or run change?", the latency gate "did the MEASURED binary move?".
pub fn lightgbm_affected(
    owners: Option<&BTreeSet<String>>,
    files: &[String],
    job_moves_lightgbm: impl FnOnce() -> bool,
) -> bool {
    let Some(owners) = owners else { return true };
    owners.intersection(&set(tables::roster::LIGHTGBM_CRATES)).next().is_some()
        || files.iter().any(|f| tables::readers::LIGHTGBM_JOB_INPUTS.contains(&f.as_str()))
        || (files.iter().any(|f| f == tables::readers::LIGHTGBM_JOB_WORKFLOW)
            && job_moves_lightgbm())
}
