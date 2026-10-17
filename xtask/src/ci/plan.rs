//! The computed plan, the ladder that computes it, and the one loud check on the graph it reads.

use std::collections::BTreeSet;
use std::path::Path;

use super::docs_content;
use super::git::{self, Env};
use super::global_parts::{self, PartPlan};
use super::graph::{self, Graph, affected_from};
use super::lockfile::cargo_lock_additive_only;
use super::ops_gates;
use super::roster::{ci_crates, doc_crates, docs_affected, nextest_filter, package_args};
use super::selection::{
    comment_only_global, datahub_affected, escalates, gate_crates_for, lane_crates_for,
    latency_affected, latency_compiled, lightgbm_affected, pack_suites, post_merge_dropped,
    tradehub_affected,
};
use super::tables::escalation::PlanOutput;
use super::tables::gate_triggers::GateTrigger;
use super::{DIAG_PREFIX, tables, workspace_deps};

/// The computed plan. `diagnostic` is stderr, everything else is stdout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub any: bool,
    /// The affected roster crates, in roster order — the tests the `test` job RUNS.
    pub ordered: Vec<String>,
    /// The `vike-ops` gates the `test` job runs: `None` = every one (the crate is selected whole, as it was
    /// before the per-gate triggers), `Some(names)` = only those test binaries. Meaningful only when `ordered`
    /// holds `vike-ops`. Computed by [`ops_gates::select`]; it narrows the `tests=` filter and nothing else, so
    /// `crates=` and `roster=` still name the whole crate.
    pub ops_gates: Option<Vec<String>>,
    /// The WHOLE derived roster ([`ci_crates`]), in roster order — what the `test` job BUILDS.
    ///
    /// ⚠ It is the same list for every change, and that constancy is the entire point of the field.
    /// cargo resolves features over the packages named on the command line, so a `-p` set that
    /// differs from the last one built on a runner is a different FEATURE RESOLUTION of the shared
    /// dependencies — a different variant of the ~400-crate tree, compiled from scratch into the
    /// same persistent `target/`. The `test` job used to build `-p {ordered}`, which differs on
    /// nearly every pull request.
    ///
    /// ⚠ **What that buys is SMALLER than the first measurement suggested, and both are recorded.**
    /// All on the latency box lanes, 2026-10-03, in `ci.yml`'s build environment (line-tables-only debuginfo,
    /// no incremental, 8 jobs, mold); the box was shared, ±35% on any single wall time.
    ///
    /// 1. SAME source, a DIFFERENT plan — the axis the problem was found on. In a target holding the
    ///    roster build, the UI plan (the files of #2395, 10 crates) through its own `-p` built in
    ///    2m 31s (128 units; the CI-speed measurement had 274.6 s with 0 sccache hits) and linted in
    ///    78.5 s; through this list, 0.65 s and 0.57 s, and a bridge plan straight after it 0.60 s.
    /// 2. A REPLAY of six consecutive merges of main (2204f65ea → 047d651d5), each form in its own
    ///    persistent target, both primed with the base's roster build. This is the axis a runner
    ///    actually lives on: the SOURCE changes between runs too, and this list then rebuilds and
    ///    relinks the whole roster's dependents of every changed crate. Wall seconds, old / new:
    ///
    /// | merge | plan | build (units compiled) | clippy | doctests |
    /// |---|---|---|---|---|
    /// | ce87b588c | 23 crates | 93 (139) / 93 (15) | 25 / 16 | 4 / 10 |
    /// | 8a7312d27 | all 68 | 243 (62) / 278 (62) | 52 / 48 | 11 / 11 |
    /// | cf3f950d8 | 10 crates | 112 (130) / 126 (21) | 46 / 19 | 2 / 10 |
    /// | 3352fe88d | all 68 | 276 (47) / 231 (43) | 44 / 49 | 15 / 14 |
    /// | 8cef55c8c | all 68 | 329 (59) / 257 (59) | 35 / 62 | 10 / 9 |
    /// | 047d651d5 | 10 crates | 155 (96) / 112 (15) | 68 / 31 | 3 / 11 |
    /// | **sum** | | **1209 / 1096** | **269 / 225** | **44 / 65** |
    ///
    ///    1522 s against 1385 s: a MODEST net win (−9%; −16% over the three narrow merges), with no
    ///    single full-plan row outside the noise. The new form compiles far fewer units on a narrow
    ///    plan (15-21 against 96-139) but its build wall is dominated by LINKING test executables,
    ///    which both forms pay — so the build is a wash per merge (cf3f950d8 lost 14 s, 047d651d5 won
    ///    43 s); clippy is where it wins; the doctests are where it loses, ~6-9 s on every narrow run
    ///    (every roster crate's doctests run, and they are not cached). The replay has ONE target per
    ///    form; CI's four runners alternate, so a runner sees more distinct plans between its own
    ///    runs than this did — which is the old form's cost, not the new one's — and the per-variant
    ///    growth of `target/` against rust-ci-setup's size cap was not measured at all.
    ///
    /// The first-ever cost — a cold target, the roster: 5m 19s (456 units) at load ~29, 10m 57s at
    /// load 64-94; clippy 1m 32s; doctests 11 s.
    ///
    /// What it does NOT change is which crates' tests run ([`Plan::ordered`], through `tests=`).
    /// What it DOES change is the configuration each runs in — always the roster's, as `just test`
    /// and every full-matrix run already had it. Measured, the UI plan ran the identical 3,858 tests
    /// both ways; the bridge plan lost none and GAINED 374 (4,737 → 5,111): tests behind features
    /// that only the roster's unification turned on (then `vike-data`'s `hist-datafusion` suites, now
    /// the crate `vike-store`'s own tests, and 12 in `vike-bridge-core`) — 178 test-seconds, ~9 s of
    /// wall at 20 threads (estimated as test-seconds over threads; the bridge run took 69-72 s in
    /// total).
    pub roster: Vec<String>,
    /// Feature-suite matrix legs (a key, or several space-joined keys packed by [`pack_suites`]), the
    /// heaviest leg first. On a `pull_request` event the [`tables::suite_rules::POST_MERGE_SUITES`]
    /// keys are not in it, except those whose own crates link a package the change moved
    /// ([`post_merge_dropped`]). On a `pull_request` opened by
    /// [`tables::suite_rules::DEPENDABOT_ACTOR`] it holds ONLY those kept guarded suites, so the
    /// `features` job runs what the plan says and nothing else. A key whose own `case` arm of the suite
    /// script moved, or every key for a moved `features:` block or script frame, joins whatever the crates
    /// say (decision 0142, [`global_parts::part_plan`]); an own-arm key stays on a pull request even when it
    /// is post-merge.
    pub suites: Vec<String>,
    /// The `datahub-all` job, the data side's one feature build ([`datahub_affected`]).
    pub datahub: bool,
    /// The `tradehub-all` job, the trading side's one feature build ([`tradehub_affected`]).
    pub tradehub: bool,
    /// The the latency box latency gate ([`latency_affected`]), ONE rule for a pull request, a push to main and
    /// the merge queue (decision 0130): a source edit of a measured crate that changes more than its
    /// comments (decision 0139), a lock entry of their normal
    /// closure, the text of `ci.yml`'s `latency:` job, the root manifest's `[profile.*]` tables, the
    /// toolchain pin, `.cargo/` or the verdict script. True when there is no file list.
    pub core: bool,
    pub app: bool,
    pub lightgbm: bool,
    /// The API-reference job. See [`docs_affected`] for why this is not a slice of `affected`.
    ///
    /// ⚠ The EVENT decides first (decision 0137): a `pull_request` plan says false unless the change
    /// edits one of the job's own inputs (`tables::readers::DOCS_JOB_INPUTS`, or the text of its own
    /// `api-docs:` block: `tables::readers::DOCS_JOB_KEY`), a `workflow_dispatch`
    /// plan says true whatever the diff, and every other event (`push`, `merge_group`, no event,
    /// `CI_FULL=1`) keeps the content answer. The job's rustdoc build and HTML scan run after the merge.
    pub docs: bool,
    /// The one-line, operator-facing account of how the diff base was chosen and how many files it
    /// produced. Without it a plan is unfalsifiable after the fact.
    pub diagnostic: String,
}

impl Plan {
    /// The `key=value` block the `plan` step turns into job outputs.
    ///
    /// ⚠ Every key stays on the ONE format line below: `crates/vike-ops/tests/release/api_docs_gate.rs`
    /// reads the emitted key set off that line, and holds both `ci.yml`'s declared outputs and
    /// `scripts/verify_branch.sh`'s `PLAN_KEYS` to it.
    pub fn render(&self) -> String {
        let suites: Vec<String> = self.suites.iter().map(|s| format!("\"{s}\"")).collect();
        let b = |v: bool| if v { "true" } else { "false" };
        format!(
            "any={}\ncrates={}\nroster={}\ntests={}\nsuites=[{}]\ndatahub={}\ntradehub={}\ncore={}\napp={}\nlightgbm={}\ndocs={}\n",
            b(self.any),
            package_args(&self.ordered),
            package_args(&self.roster),
            nextest_filter(&self.ordered, self.ops_gates.as_deref()),
            suites.join(", "),
            b(self.datahub),
            b(self.tradehub),
            b(self.core),
            b(self.app),
            b(self.lightgbm),
            b(self.docs),
        )
    }
}

/// Compute the plan. `env` is the caller's environment and `cwd` the repository root — both
/// PARAMETERS rather than `std::env`/CWD reads, which is what lets
/// `xtask/tests/ci_plan_gate.rs` drive the whole ladder over a planted git topology
/// in-process, with no ambient CI variable able to change what a scenario measures.
pub fn compute(env: &Env, cwd: &Path) -> Result<Plan, String> {
    compute_with(env, cwd, tables::gate_triggers::GATE_TRIGGERS)
}

/// [`compute`] over an explicit set of gate-trigger rows. `compute` is this over
/// [`tables::gate_triggers::GATE_TRIGGERS`]; the planner's own tests pass planted rows, so what they assert
/// about the mechanism does not move when the real rows do.
pub fn compute_with(env: &Env, cwd: &Path, triggers: &[GateTrigger]) -> Result<Plan, String> {
    let g = load_graph_checked(cwd)?;
    let roster = ci_crates(&g.names);

    // ONE base for the whole run: `cargo_lock_additive_only` reads the same commit's Cargo.lock, and
    // a plan whose file list and whose lock comparison came from two different bases is incoherent.
    // CI_FULL short-circuits the resolution entirely rather than resolving a base and discarding it:
    // release.yml sets it on a TAG checkout, where any base this ladder invented would be fiction,
    // and printing one would read as the reason for a full matrix that was asked for outright.
    let full = git::var(env, "CI_FULL") == "1";
    let (base, how, files): (Option<String>, String, Option<Vec<String>>) = if full {
        (None, "CI_FULL=1 — the FULL matrix, asked for explicitly".to_string(), None)
    } else {
        match git::resolve_base(env, cwd) {
            Some((base, how)) => {
                let files = git::diff_names(&base, cwd);
                (Some(base), how, files)
            }
            None => (None, "base UNRESOLVABLE — escalating to the FULL matrix".to_string(), None),
        }
    };
    let detail = match &files {
        None => "no narrow file list".to_string(),
        Some(f) => format!("{} changed file(s)", f.len()),
    };
    let diagnostic = format!("{DIAG_PREFIX} {how} -> {detail}");

    // Whether `vike-ops` runs EVERY gate: true unless the change is one the rows and the gates' links can
    // judge (set in the narrow arm below; no file list is the full matrix). `graph_reach` is the other half
    // of that judgement: every crate with an edit that can be linked, and everything that depends on one of
    // them normally; `ops_gates::select` meets it with each gate's `links`.
    let mut ops_everything = true;
    let mut graph_reach: BTreeSet<String> = BTreeSet::new();
    // The LightGBM job's crate input: the crates that OWN a changed file plus the members a moved
    // package reaches, never the global expansion ([`lightgbm_affected`]). `None` = no file list.
    let mut lightgbm_owners: Option<BTreeSet<String>> = None;
    // What the part-judged global files fire (decision 0142): `ci.yml` by job block, the suite script by
    // `case` arm. Empty with no file list (everything fires then anyway).
    let mut parts = PartPlan::default();
    // `lat` is the latency gate's answer, `core` (one rule for every event: decision 0130).
    let (affected, closure, direct, structural, compiled, lat, docs, lane, moved) = match &files {
        // CI_FULL is the manual escape hatch; `files is None` = an unresolvable diff base. No
        // trustworthy file list means no way to reason about the hot fold either, so the latency gate
        // fires for the same reason everything else does: err toward measuring.
        None => (
            g.names.clone(),
            g.names.clone(),
            g.names.clone(),
            g.names.clone(),
            g.names.clone(),
            true,
            true,
            BTreeSet::new(),
            BTreeSet::new(),
        ),
        Some(files) => {
            // The global files whose WHOLE change is `#` comments and blank lines (`base` is `Some`
            // whenever `files` is): not global edits. They join the lane through the gate crate
            // below, and are dropped from the latency question — nothing else.
            let commented: BTreeSet<&String> = files
                .iter()
                .filter(|f| comment_only_global(f, base.as_deref().unwrap_or_default(), cwd))
                .collect();
            // The global files judged PART BY PART (decision 0142): an edit `global_parts` can split is
            // no global edit. It fires the jobs whose block (or the suites whose arm) it moved, applied
            // below, and joins the lane like an exempted file; every doubt leaves it global.
            parts = global_parts::part_plan(files, &commented, base.as_deref(), cwd);
            let mut global: BTreeSet<String> = files
                .iter()
                .filter(|f| {
                    escalates(f, cwd) && !commented.contains(f) && !parts.files.contains(f.as_str())
                })
                .cloned()
                .collect();
            // EXCEPT a Cargo.lock-ONLY global change that is purely additive: it cannot move a
            // resolved version, so the narrow selection from the accompanying source changes holds.
            let lock_only = global.len() == 1 && global.contains("Cargo.lock");
            if lock_only && cargo_lock_additive_only(base.as_deref().unwrap_or_default(), cwd) {
                global.clear();
            }
            // EXCEPT a root-manifest change confined to `[workspace.dependencies]` (with its
            // `Cargo.lock`, and nothing else global): it reaches the members that link the packages it
            // moves, not the roster. `workspace_deps::consumers` answers `None` for every other shape.
            let dep_only = global.iter().all(|f| f == "Cargo.toml" || f == "Cargo.lock");
            let dep_consumers = if dep_only && global.contains("Cargo.toml") {
                workspace_deps::consumers(files, base.as_deref().unwrap_or_default(), cwd, &g)
            } else {
                None
            };
            let dep_edit = dep_consumers.is_some();
            if dep_edit {
                global.clear();
            }
            // The members a MOVED package reaches, for the one question a pull request still asks of a
            // dependency move: which [`tables::suite_rules::POST_MERGE_SUITES`] it keeps
            // ([`post_merge_dropped`]). It is `dep_consumers` itself when the change is dependency-only, so
            // the direct suites and this exception read the SAME set; it is answered from the root files
            // alone ([`workspace_deps::moved_consumers`]) when the change escalates anyway — a lockfile
            // with no manifest edit, or a dependency edit beside another global file — so an escalation
            // still keeps the suites its package reaches and no others.
            let moved = dep_consumers.clone().unwrap_or_else(|| {
                let base = base.as_deref().unwrap_or_default();
                workspace_deps::moved_consumers(files, base, cwd, &g).unwrap_or_default()
            });
            // The lane: what the changed files' readers need, plus the gate crate for a comment-only
            // edit of a global file ([`comment_only_global`] says why it is not nothing), for a
            // dependency edit (the root manifest and the lockfile are text its gates read) or for a
            // part-judged edit (its gates read the whole file).
            let mut lane = lane_crates_for(files);
            if !commented.is_empty() || dep_edit || !parts.files.is_empty() {
                lane.extend(
                    tables::readers::EXEMPT_INPUT_GATE_CRATES.iter().map(|c| (*c).to_string()),
                );
            }
            // TWO seed sets, because a changed file's blast radius depends on whether anything can
            // LINK it. `changed` is every crate with a changed file — what the crate itself must be
            // tested for. `linkable` drops the crates whose only changed files are TEST-TARGET
            // SOURCES (`graph::is_test_target_source`): cargo compiles those into their own
            // integration-test / bench / example binaries, no `[dependencies]` edge can reach one,
            // and a dependent building this crate's lib never compiles them — so walking their
            // reverse-dep world selects crates that cannot observe the change.
            //
            // MEASURED before it was written (the reopening condition
            // `docs/decisions/0004-crate-splits-do-not-shrink-ci.md` names — a FINER selection
            // mechanism, not a crate split): over `main`'s last 200 commits it changes the answer
            // for the gate-fix / ops class, whose files are all under `tests/`. PR #1409 is the
            // shape — a one-row edit to `crates/vike-boot/tests/one_owner.rs` selected 6 crates and
            // 6 feature suites, and selects 1 crate and 0 suites here.
            let mut changed: BTreeSet<String> = BTreeSet::new();
            let mut linkable: BTreeSet<String> = BTreeSet::new();
            // The crates whose MANIFEST or BUILD SCRIPT changed: a feature or structure edit, which is
            // the only kind of edit the structural suites (`tables::suite_rules::MANIFEST_CLOSURE_SUITES`) look for.
            let mut manifest: BTreeSet<String> = BTreeSet::new();
            // A THIRD kind of file: a `test-support` module, compiled only under `cfg(test)` or with
            // its crate's feature F on (`graph::feature_test_module`). The builds that compile it are
            // exactly those F is on in, so when only DEV edges turn F on, its crate goes to `changed`
            // and not `linkable`, as for `cfg(test)`, and those dev dependents are planned ONE hop
            // (`enablers`: tested, never walked through). Their tests are what can see the edit.
            // Unlike a `cfg(test)` edit, the crate stays what a dependency-only suite trigger is
            // matched on (`feature_owners`, into `compiled` below): such a suite's arm may build a
            // dev enabler's tests, and then it compiles the module. Latency still fires for it (it is not
            // test-only to [`latency_compiled`]: the measured binary turns F on). A NORMAL edge turning F on links the module into a shipped
            // build, so the crate is `linkable` and the plan is a production edit's, whole.
            //
            // MEASURED (the latency box lane, 2026-10-07, one appended comment line, `affected` on 91602b328's
            // tree): `crates/vike-model/src/libm_walk.rs` planned 57 crates with hist, app, lightgbm,
            // docs and core all true; with this rule, 20 (vike-model and its 19 dev enablers), with
            // `multicall`/`workspace-bins` still fired (vike-backtest and vike-cli are enablers) and
            // core and lightgbm still true. A production file of the same crate (`fair.rs`), a
            // `cfg(test)` module and a test-target source planned byte-identically before and after.
            let mut enablers: BTreeSet<String> = BTreeSet::new();
            // The latency gate's crate input: the crates that OWN a changed file that can reach the
            // measured binary ([`latency_compiled`]: every file but a test-only one, the harness's own
            // files excepted, or a `.rs` file of a measured crate whose edit changes only comments,
            // decision 0139). No `dep_consumers` member joins it: a root-manifest dependency
            // edit reaches the measured binary only through the lock it resolves (decision 0130).
            let mut lat_changed: BTreeSet<String> = BTreeSet::new();
            // The crate half of the latency rule is met with the measured crates DERIVED from the graph
            // (the four and the workspace crates in their normal closure).
            let measured = super::selection::measured_crates(&g.normal_closure, &g.names);
            // The crate sources whose WHOLE edit is comments (decision 0139): one diff and one blob batch
            // for the change ([`git::rust_comment_only_files`]), asked of every crate-owned `.rs` file but
            // a build script (`build.rs`, `build/`), which keeps today's rules. Such a file is no latency
            // input. Unless some other build or test reads it as TEXT (`tables::readers::SOURCES_READ_AS_TEXT`:
            // text is data a reader sees, though no measured instruction), it also seeds no
            // reverse-dependency walk, no job and no suite: its crate joins the LANE only, so its own tests
            // run (they may read its sources), every `.rs` edit still plans the gate crates
            // (`gate_crates_for`), the `test` job still lints and doctests the whole roster, and the
            // `api-docs` content rule still reads its diff.
            let candidates: Vec<&str> = files
                .iter()
                .filter(|f| f.ends_with(".rs"))
                .filter(|f| {
                    graph::owner_of(f, &g.dirs, cwd)
                        .is_some_and(|(_, rel)| rel != "build.rs" && !rel.starts_with("build/"))
                })
                .map(String::as_str)
                .collect();
            let commented_sources =
                git::rust_comment_only_files(base.as_deref().unwrap_or_default(), &candidates, cwd);
            let mut feature_owners: BTreeSet<String> = BTreeSet::new();
            // The owners among them whose dev enablers include `vike-ops`: its gates compile such a module,
            // so the edit is one the gates' links must be asked about.
            let mut ops_enabled_owners: BTreeSet<String> = BTreeSet::new();
            // `(crate, rel, path)` of every file that made its crate linkable, or would have but for a
            // comment-only edit: what the API-reference trigger reads the diff of ([`docs_affected`]).
            let mut doc_edits: Vec<(String, String, String)> = Vec::new();
            for f in files {
                let Some((name, rel)) = graph::owner_of(f, &g.dirs, cwd) else { continue };
                let dir = g
                    .dirs
                    .iter()
                    .find(|(_, n)| n.as_str() == name)
                    .map(|(d, _)| Path::new(d.as_str()));
                // A crate-owned DOCUMENT (`.md`): compiled into nothing, so it is never linkable and no
                // latency input. The crate stays in `changed`, whose own tests may read the page.
                let document = graph::is_crate_document(&rel);
                // Test-only by LAYOUT (`tests/`, `benches/`, `examples/`) or by CODE: a module the
                // declaring file puts behind `#[cfg(test)]`, which no dependent's build compiles.
                let test_only = document
                    || graph::is_test_target_source(&rel)
                    || dir.is_some_and(|d| graph::is_cfg_test_module(d, &rel));
                let comment_only = commented_sources.contains(f.as_str());
                // The selection half drops it only when no other crate reads its text.
                let dropped =
                    comment_only && !tables::readers::SOURCES_READ_AS_TEXT.contains(&f.as_str());
                if !test_only {
                    match dir.and_then(|d| graph::feature_test_module(d, &rel)) {
                        Some(feature) => {
                            let (dev, normal) = g.feature_enablers(&name, &feature);
                            if normal.is_empty() {
                                if !dropped {
                                    if dev.contains(ops_gates::OPS_CRATE) {
                                        ops_enabled_owners.insert(name.clone());
                                    }
                                    enablers.extend(dev);
                                    feature_owners.insert(name.clone());
                                }
                            } else {
                                if !dropped {
                                    linkable.insert(name.clone());
                                }
                                doc_edits.push((name.clone(), rel.clone(), f.clone()));
                            }
                        }
                        None => {
                            if !dropped {
                                linkable.insert(name.clone());
                            }
                            doc_edits.push((name.clone(), rel.clone(), f.clone()));
                        }
                    }
                }
                // A build script may declare modules (`#[path = "build/x.rs"] mod x;`), so `build/`
                // is part of it: an edit there is a BUILD SCRIPT edit.
                if rel == "Cargo.toml" || rel == "build.rs" || rel.starts_with("build/") {
                    manifest.insert(name.clone());
                }
                if latency_compiled(f, test_only, || comment_only) {
                    lat_changed.insert(name.clone());
                }
                if dropped {
                    lane.insert(name);
                } else {
                    changed.insert(name);
                }
            }
            // The members that link a moved package: tested like a changed crate, and a MANIFEST
            // edit for the structural suites — a dependency or feature edit is what they look for.
            for member in dep_consumers.iter().flatten() {
                linkable.insert(member.clone());
                manifest.insert(member.clone());
                changed.insert(member.clone());
            }
            // `changed` before any escalation, plus the members a moved package reaches when the change
            // escalates anyway (a lockfile with no manifest edit, a dependency edit beside `deny.toml`):
            // a global file alone fires nothing here (decision 0138).
            lightgbm_owners = Some(changed.union(&moved).cloned().collect());
            // Computed from `files`/`lat_changed` directly, NEVER from `affected` — the whole point is
            // that the escalation below must not be able to buy a microsecond measurement.
            //
            // ⚠ `lat_changed`, deliberately NOT `changed` or `linkable`: a test-only file (a test-target
            // source, a `cfg(test)` module) is compiled into no dependent's build and not into the
            // measured binary, so it seeds nothing; but the gate's own harness
            // (`crates/vike-core/tests/runtime_latency.rs` and the files it compiles in) IS a test-target
            // source, so [`latency_compiled`] keeps `tables::roster::LATENCY_HARNESS_FILES` in: narrowing
            // those would let a change to how the p99 is MEASURED ship without re-measuring it. A
            // `test-support` module (`graph::feature_test_module`) is NOT test-only here: the measured
            // binary links vike-exec, vike-model and vike-data with `test-support` on (vike-core's dev
            // edges, unified across the test graph), so it compiles into it. The production rule is
            // unchanged; only test-only edits stop firing, and
            // `xtask/tests/ci_plan_gate/latency_test_edits.rs` pins both halves. A `.rs` edit that
            // changes only comments stops firing too (decision 0139),
            // `xtask/tests/ci_plan_gate/latency_comment_edits.rs` pins it.
            // A `#`-comment-only edit of a latency input file (`.cargo/config.toml`) is no input. The
            // gate's workflow and the root manifest stay in whatever their edit: they are judged by
            // the TEXT of the job block and of the `[profile.*]` tables, where a comment counts.
            let workflow = tables::escalation::LATENCY_JOB_WORKFLOW;
            let latency_files: Vec<String> = files
                .iter()
                .filter(|f| !commented.contains(f) || f.as_str() == workflow || *f == "Cargo.toml")
                .cloned()
                .collect();
            // The crate half is met with `measured` (above); a `Cargo.lock` edit is judged by the
            // entries it changes against that closure, a workflow edit by the gate's job block and a
            // root manifest edit by its `[profile.*]` tables, every text from git at the ONE base of
            // this run (each question fires on any doubt).
            let at_base = |path: &str| base.as_deref().and_then(|b| git::show_at(b, path, cwd));
            let core = latency_affected(
                &latency_files,
                &lat_changed,
                &measured,
                || {
                    let new = git::show_at("HEAD", "Cargo.lock", cwd);
                    super::selection::latency_lock_moved(
                        at_base("Cargo.lock").as_deref(),
                        new.as_deref(),
                        &g.normal_closure,
                    )
                },
                || {
                    let new = git::show_at("HEAD", workflow, cwd);
                    super::selection::latency_job_moved(
                        at_base(workflow).as_deref(),
                        new.as_deref(),
                    )
                },
                || {
                    let new = git::show_at("HEAD", "Cargo.toml", cwd);
                    super::selection::latency_profile_moved(
                        at_base("Cargo.toml").as_deref(),
                        new.as_deref(),
                    )
                },
            );
            // Same two inputs, a different question — see [`docs_affected`]. A GLOBAL file forces
            // it for the reason it forces everything else: the root manifest, the toolchain and the
            // lockfile all reach every crate's rendered pages, and a file list this run cannot
            // reason about is not evidence that the reference is unchanged.
            // ...plus the job's OWN inputs, which own no crate and so are invisible to both terms
            // above. `docs_affected` asks which crate's rendered pages moved; `!global.is_empty()`
            // asks whether this run can reason about the file list at all. A change to the SCRIPT
            // THAT BUILDS the reference answers neither, and rode the `scripts/` escalation until
            // `tables::escalation::GLOBAL_EXEMPT_FILES` stopped escalating it — at which point this job, the
            // only thing that ever EXECUTES that script, would have stopped firing on its own
            // change class. See `tables::readers::DOCS_JOB_INPUTS`.
            // ...and since decision 0133 an edit of a documented crate's own file fires only when its
            // diff can move a rendered page (`docs_content::edit_moves_docs`, read lazily, file by
            // file); a dependency-only root edit fires through its documented consumers, as the root
            // manifest it is; a file a documented crate includes into its docs fires by name.
            let docs_base = base.as_deref().unwrap_or_default();
            let documented: BTreeSet<String> = doc_crates(&g.names).into_iter().collect();
            let docs = !global.is_empty()
                || files.iter().any(|f| {
                    tables::readers::DOCS_JOB_INPUTS.contains(&f.as_str())
                        || tables::readers::DOCS_INCLUDED_FILES.contains(&f.as_str())
                })
                || dep_consumers.iter().flatten().any(|m| documented.contains(m))
                || docs_affected(&doc_edits, &g.names, |path, rel| {
                    docs_content::edit_moves_docs(docs_base, path, rel, cwd)
                });
            // Who the graph alone reaches, BEFORE the whole-tree gate crates are added below.
            let mut reach = affected_from(&linkable, &g.radj, &g.radj_dev);
            // The question `vike-ops` asks of an edit is not "does the graph reach me" (it nearly always does
            // through `vike-model`) but "does it reach a crate one of my gates LINKS": the gates are test
            // binaries and link only the crates their code names (`GateTrigger::links`). So the edits are
            // walked on their own, over NORMAL edges only, as far as a linked crate can follow them: a dev
            // edge is a test edge, and a gate does not link the tests of a crate it names.
            let mut graph_edits = linkable.clone();
            graph_edits.extend(ops_enabled_owners);
            for e in &graph_edits {
                graph_reach.extend(graph::transitive_rdeps(e, &g.radj));
            }
            // A root-manifest dependency edit keeps the old answer: the gates link third-party crates too
            // (`toml`, `tempfile`, `proc-macro2`), which no `links` list names, so a moved package that
            // reaches `vike-ops` can change any of them.
            let dep_reaches_ops = dep_consumers.as_ref().is_some_and(|c| {
                affected_from(c, &g.radj, &g.radj_dev).contains(ops_gates::OPS_CRATE)
            });
            // A moved `test:` block runs every gate: the job's own steps changed (decision 0142).
            ops_everything =
                !global.is_empty() || dep_reaches_ops || parts.outputs.contains(&PlanOutput::Tests);
            reach.extend(enablers);
            // What a trigger a job only COMPILES as a dependency is matched against
            // (`tables::roster::DATAHUB_ALL_DEPENDENCY_CRATES`): the crates with an edit that can LINK, which is
            // `linkable` — a test-only edit to such a crate changes nothing the job builds — plus
            // the crates of a `test-support` module edit, which a dev enabler's test build links.
            let compiled: BTreeSet<String> = if global.is_empty() {
                linkable.union(&feature_owners).cloned().collect()
            } else {
                g.names.clone()
            };
            // The closure of every edit something can BUILD: the graph's reach, the crates of a linkable
            // or `test-support` edit (`compiled`), and the gate crates. It is `affected` MINUS the crates
            // whose only changed files are `tests/`/`benches/`/`examples/` sources or `cfg(test)` modules —
            // what the `tradehub-all` job's closure triggers are matched against
            // (`tables::roster::TRADEHUB_ALL_CLOSURE_CRATES`), because the job compiles no such file except
            // in the crates it names, which are matched on `direct` ([`tradehub_affected`]).
            let closure = if global.is_empty() {
                let mut c = reach;
                c.extend(compiled.iter().cloned());
                c.extend(gate_crates_for(files));
                c
            } else {
                g.names.clone()
            };
            let affected = if global.is_empty() {
                // The dev hop rides `linkable` too: a crate whose TESTS use X is affected when X's
                // LIB moves, and a test-target source is not X's lib.
                let mut a = closure.clone();
                a.extend(changed.iter().cloned());
                a
            } else {
                g.names.clone()
            };
            // The set a feature suite is matched against (outside
            // `tables::suite_rules::MANIFEST_CLOSURE_SUITES`), and the named crates of the feature jobs: the
            // crates that OWN a changed file, with no reverse-dep walk — and EVERY crate when a global file
            // changed, for the same reason `affected` is every crate then.
            let direct = if global.is_empty() { changed.clone() } else { g.names.clone() };
            // What a structural suite is matched against: a direct edit, OR a manifest/build-script
            // edit anywhere below it. `tables::suite_rules::MANIFEST_CLOSURE_SUITES` says why that, and not the
            // whole closure.
            let structural = if global.is_empty() {
                let mut s = changed.clone();
                s.extend(affected_from(&manifest, &g.radj, &g.radj_dev));
                s
            } else {
                g.names.clone()
            };
            (affected, closure, direct, structural, compiled, core, docs, lane, moved)
        }
    };
    let core = lat;

    // `affected` is the full workspace-crate set touched (used for the closure suites and `app`);
    // `ordered` is the subset the `test` job runs — plus ONE force-add that joins the LANE without
    // joining `affected`: the crates whose tests READ a changed file that owns no crate
    // (`lane_crates_for`). Deliberately lane-only — feeding it into `affected` would fire the
    // feature matrix those crates trigger, to buy a test run.
    //
    // ⚠ There were others, each deleted with its reason. Every `.rs` change force-added vike-data,
    // for the store-kind gate that lived in its `tests/` — under the roster build that ran all of
    // vike-data's DataFusion suites on every `.rs` PR (~600-650 test-seconds, MEASURED); the gate
    // moved to vike-ops (`crates/vike-ops/tests/venues/store_kind_gate.rs`), which `gate_crates_for`
    // already selects for any `.rs` file, and the rule went with it (2026-10-03). And a selected
    // binary-spawning crate force-added the crates whose binaries it spawns, to get them BUILT —
    // which the roster build now does on every run (`tables::roster::BINARY_DRIVER_COMPANIONS` carries the
    // measurement), so all that rule still bought was running the companions' own tests on a change
    // that could not touch them.
    //
    // A FILTER over the roster, never a list built beside it: every crate `tests=` names must be in
    // the build `roster=` names, or nextest selects nothing from it without a word (see
    // `nextest_filter`).
    //
    // A moved `test:` block of `ci.yml` (decision 0142) runs the job over the whole roster: the edited
    // steps are what every crate's tests run through.
    let fires = |o: PlanOutput| parts.outputs.contains(&o);
    let ordered: Vec<String> = roster
        .iter()
        .filter(|c| fires(PlanOutput::Tests) || affected.contains(*c) || lane.contains(*c))
        .cloned()
        .collect();
    // Which of `vike-ops`'s gates run (`ops_gates`' module doc is the rule list). `None` = all of them:
    // no file list, a crate it links changed, or any doubt inside `ops_gates::select`.
    let ops_gates = match &files {
        Some(files) if !ops_everything && ordered.iter().any(|c| c == ops_gates::OPS_CRATE) => {
            narrowed_ops_gates(files, base.as_deref(), cwd, &g, triggers, &graph_reach)
        }
        _ => None,
    };
    // A suite fires when a crate it names owns a changed file — or, for a structural suite, when a
    // manifest or build script below it changed. Declaration order: this list is the matrix order.
    // A key the part-judged files fire (decision 0142: a moved `features:` block or script frame fires
    // every key, a moved `case` arm its own) is affected whatever the crates.
    let affected_keys: Vec<String> = tables::feature_suites::FEATURE_SUITES
        .iter()
        .filter(|(k, triggers)| {
            if parts.suites.contains(*k) {
                return true;
            }
            let seed = if tables::suite_rules::MANIFEST_CLOSURE_SUITES.contains(k) {
                &structural
            } else {
                &direct
            };
            triggers.iter().any(|t| seed.contains(*t))
        })
        .map(|(k, _)| (*k).to_string())
        .collect();
    // The PULL-REQUEST plan leaves out [`tables::suite_rules::POST_MERGE_SUITES`]; every other event
    // keeps them, and so does `CI_FULL=1` (the release). The EVENT decides, and the diff only through
    // ONE door: a global escalation or an unresolvable base on a pull request fires every other suite and
    // still not these four, because they are post-merge by rule (the `push` runs and the release run
    // them) — EXCEPT the suites whose own crates link a package the change moved (`moved`, see
    // [`post_merge_dropped`]): the Dependabot guard, `png-export` above all. Dropped BEFORE packing, so
    // the packer sees only the suites a pull request actually schedules.
    let event = git::var(env, "GITHUB_EVENT_NAME");
    let pull_request = !full && event == "pull_request";
    // A suite whose OWN `case` arm moved stays too (decision 0142): the arm is the suite's own machinery,
    // and its leg is the only job that runs it, as the `api-docs` job's own inputs plan it (0137).
    let affected_keys: Vec<String> = affected_keys
        .into_iter()
        .filter(|k| {
            !(pull_request && post_merge_dropped(k, &moved) && !parts.own_arms.contains(k.as_str()))
        })
        .collect();
    // A DEPENDABOT pull request runs the guarded suites and no other: a bump does not need the full
    // per-feature matrix (a `Cargo.lock` change makes the affected set every crate), and what it does
    // need is the post-merge suites the guard just kept. `CI_FULL=1` is not a pull request here, so it
    // fires everything whatever the actor; an unresolvable base kept no suite, so this plan is empty.
    // `GITHUB_ACTOR` is set by Actions for every step, as `GITHUB_EVENT_NAME` is.
    let dependabot =
        pull_request && git::var(env, "GITHUB_ACTOR") == tables::suite_rules::DEPENDABOT_ACTOR;
    let affected_keys: Vec<String> = affected_keys
        .into_iter()
        .filter(|k| !dependabot || tables::suite_rules::POST_MERGE_SUITES.contains(&k.as_str()))
        .collect();
    let suites = pack_suites(&affected_keys);

    // The API-reference job is POST-MERGE (decision 0137, amending 0133): its rustdoc build and its scan
    // of the built HTML leave the pull request, by EVENT, as the `POST_MERGE_SUITES` did (0124). What a
    // pull request keeps is the cheap half, in the `test` job: `vike-ops`'s `api_docs_gate` runs the
    // scan's `--selftest`, and its `publish_mirror_gate` runs the same `publish_mirror.sh --dry-run`
    // source scan the job's stage 1 runs. The exceptions:
    //   * a pull request that edits one of the job's OWN inputs still plans the job: a `DOCS_JOB_INPUTS`
    //     file (the build script, the mirror script it calls, the token set), or the text of the job's
    //     own `api-docs:` block in `DOCS_JOB_WORKFLOW` (`job_moved`, the latency gate's block reader,
    //     fail safe: a block it cannot find on either side fires). That job is the only thing that
    //     EXECUTES the build script, and `xtask/tests/ci_plan_gate/workflow_scripts.rs` counts it as what
    //     covers those exempted scripts on their own change; the 0125 shape (what moves the job's own
    //     machinery brings it back), on edits no pull request of the measured window made.
    //   * `workflow_dispatch` plans it whatever the diff: run on main's tip its diff is empty, and
    //     on demand is the one way to build the reference in CI without a merge.
    // An unresolvable base on a pull request (`files` is `None`) plans no job: the event decides.
    let own_inputs = || {
        files.iter().flatten().any(|f| {
            tables::readers::DOCS_JOB_INPUTS.contains(&f.as_str())
                || (f.as_str() == tables::readers::DOCS_JOB_WORKFLOW && {
                    let wf = tables::readers::DOCS_JOB_WORKFLOW;
                    let old = base.as_deref().and_then(|b| git::show_at(b, wf, cwd));
                    let new = git::show_at("HEAD", wf, cwd);
                    super::selection::job_moved(
                        old.as_deref(),
                        new.as_deref(),
                        tables::readers::DOCS_JOB_KEY,
                    )
                })
        })
    };
    // ...and on every event, a moved `api-docs:` block of `ci.yml` (on a pull request `own_inputs`
    // already says so; on a push the narrowed workflow no longer forces `docs` as a global file).
    let docs = (if pull_request { own_inputs() } else { docs || event == "workflow_dispatch" })
        || fires(PlanOutput::Docs);

    // Every output a moved `ci.yml` job block fires is ORed in below (decision 0142); for `core`,
    // `lightgbm` and `docs` that adds nothing their own block rules do not already say.
    Ok(Plan {
        any: !ordered.is_empty(),
        // The DIRECT set for the crates the job tests, the linkable one for the crates it only compiles,
        // and the raw file list for its non-crate inputs ([`datahub_affected`] says why each).
        datahub: datahub_affected(&direct, &compiled, files.as_deref().unwrap_or_default())
            || fires(PlanOutput::Datahub),
        // The DIRECT set for the crates the job tests, the closure of what something can build for the
        // top of the stack it compiles ([`tradehub_affected`]).
        tradehub: tradehub_affected(&direct, &closure) || fires(PlanOutput::Tradehub),
        core: core || fires(PlanOutput::Core),
        app: affected.contains(tables::roster::APP_CHECK_CRATE) || fires(PlanOutput::App),
        // The owners of a changed file, the raw file list and the text of the job's own `ci.yml` block
        // (read at the ONE base of this run, fail safe), never `affected` nor the global expansion of
        // `direct` ([`lightgbm_affected`] says why).
        lightgbm: lightgbm_affected(
            lightgbm_owners.as_ref(),
            files.as_deref().unwrap_or_default(),
            || {
                let wf = tables::readers::LIGHTGBM_JOB_WORKFLOW;
                let old = base.as_deref().and_then(|b| git::show_at(b, wf, cwd));
                let new = git::show_at("HEAD", wf, cwd);
                super::selection::job_moved(
                    old.as_deref(),
                    new.as_deref(),
                    tables::readers::LIGHTGBM_JOB_KEY,
                )
            },
        ) || fires(PlanOutput::Lightgbm),
        docs,
        ops_gates,
        ordered,
        roster,
        suites,
        diagnostic,
    })
}

/// The `vike-ops` gates a change selects, or `None` for all of them: [`ops_gates::select`] over this
/// change's files, with the crate's own `[[test]]` rows read from the checkout being planned, the crates
/// the change's linkable edits reach (`graph_reach`) and the diff text read lazily from the same base the
/// file list came from.
fn narrowed_ops_gates(
    files: &[String],
    base: Option<&str>,
    cwd: &Path,
    g: &Graph,
    triggers: &[GateTrigger],
    graph_reach: &BTreeSet<String>,
) -> Option<Vec<String>> {
    let base = base?;
    let dir = g.dirs.iter().find(|(_, n)| n.as_str() == ops_gates::OPS_CRATE).map(|(d, _)| d)?;
    let gates = ops_gates::crate_gates(Path::new(dir))?;
    let ops_rel: Vec<String> = files
        .iter()
        .filter_map(|f| graph::owner_of(f, &g.dirs, cwd))
        .filter(|(name, _)| name == ops_gates::OPS_CRATE)
        .map(|(_, rel)| rel)
        .collect();
    let chosen = ops_gates::select(files, &ops_rel, &gates, triggers, graph_reach, || {
        git::diff_patch(base, cwd)
    })?;
    // Every gate selected is not a narrowing: say `None`, so the filter keeps the whole package (the
    // crate's library unit tests ride the package term, and a whole-package plan is the one that was
    // measured before the triggers existed).
    if gates.iter().all(|gt| chosen.contains(&gt.name)) {
        return None;
    }
    // Manifest order, not alphabetical: the filter reads like the manifest and stays stable when a
    // gate is renamed into another folder.
    Some(gates.into_iter().map(|gt| gt.name).filter(|n| chosen.contains(n)).collect())
}

/// [`graph::load_graph`] plus the sanity checks that must be LOUD rather than a silent disarm.
///
/// [`tables::roster::LIGHTGBM_CRATES`] is a hand-written set of crate NAMES, and the job it gates exists
/// precisely because a suite that quietly runs nothing looks exactly like a suite that passes. Rename
/// or delete one of those crates and the intersection would simply stop matching: the job would never
/// fire again, no output would change, and nothing would be red. [`tables::roster::LATENCY_CRATES`]
/// carries the same check since decision 0130: the gate fires on a direct edit of the measured crates
/// only, so a renamed one would stop it firing on its own edits. The DATAHUB and TRADEHUB tables are
/// narrow too (a few named crates each), and are held by a test rather than by a refusal here:
/// `xtask/tests/ci_plan_gate/lane_directness.rs`'s `every_feature_job_crate_is_a_workspace_member`.
fn load_graph_checked(cwd: &Path) -> Result<Graph, String> {
    let g = graph::load_graph(cwd)?;
    let unknown: Vec<&str> =
        tables::roster::LIGHTGBM_CRATES.iter().copied().filter(|c| !g.names.contains(*c)).collect();
    if !unknown.is_empty() {
        return Err(format!(
            "LIGHTGBM_CRATES names {unknown:?}, which are not workspace members. The the CI box LightGBM \
             job would silently never fire again. Update xtask::ci::tables::roster::LIGHTGBM_CRATES (and the \
             suites in scripts/ci_lightgbm_suite.sh) to the new names."
        ));
    }
    let unknown: Vec<&str> =
        tables::roster::LATENCY_CRATES.iter().copied().filter(|c| !g.names.contains(*c)).collect();
    if !unknown.is_empty() {
        return Err(format!(
            "LATENCY_CRATES names {unknown:?}, which are not workspace members. The the latency box latency \
             gate would silently stop firing on their edits. Update \
             xtask::ci::tables::roster::LATENCY_CRATES to the crates the gate's binary measures."
        ));
    }
    Ok(g)
}
