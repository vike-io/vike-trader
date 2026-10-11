//! The computed plan, the ladder that computes it, and the one loud check on the graph it reads.

use std::collections::BTreeSet;
use std::path::Path;

use super::git::{self, Env};
use super::graph::{self, Graph, affected_from};
use super::lockfile::cargo_lock_additive_only;
use super::ops_gates;
use super::roster::{ci_crates, docs_affected, nextest_filter, package_args};
use super::selection::{
    comment_only_global, escalates, gate_crates_for, lane_crates_for, latency_affected,
    latency_compiled, latency_direct, lightgbm_affected, pack_suites, suite_keys_for,
};
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
    /// that only the roster's unification turns on (`vike-data`'s `hist-datafusion` suites and 12
    /// in `vike-bridge-core`) — 178 test-seconds, ~9 s of wall at 20 threads (estimated as
    /// test-seconds over threads; the bridge run took 69-72 s in total).
    pub roster: Vec<String>,
    /// Feature-suite matrix legs (a key, or several space-joined keys packed by [`pack_suites`]),
    /// ordered by each leg's first key's [`tables::feature_suites::FEATURE_SUITES`] declaration position.
    pub suites: Vec<String>,
    pub hist: bool,
    /// The the latency box latency gate: the full rule ([`latency_affected`]), which decides on `push` to main.
    pub core: bool,
    /// The narrower question ([`latency_direct`]): the change edits a measured crate's own files, the
    /// lockfile or the gate's verdict script. `ci.yml`'s `latency` job reads it on `pull_request`
    /// events only, ANDed with `core`; true when there is no file list.
    pub core_direct: bool,
    pub app: bool,
    pub lightgbm: bool,
    /// The API-reference job. See [`docs_affected`] for why this is not a slice of `affected`.
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
            "any={}\ncrates={}\nroster={}\ntests={}\nsuites=[{}]\nhist={}\ncore={}\ncore_direct={}\napp={}\nlightgbm={}\ndocs={}\n",
            b(self.any),
            package_args(&self.ordered),
            package_args(&self.roster),
            nextest_filter(&self.ordered, self.ops_gates.as_deref()),
            suites.join(", "),
            b(self.hist),
            b(self.core),
            b(self.core_direct),
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
    // `lat` is the latency pair `(core, core_direct)`, destructured right after the match.
    let (affected, closure, direct, structural, compiled, lat, docs, lane, forced) = match &files {
        // CI_FULL is the manual escape hatch; `files is None` = an unresolvable diff base. No
        // trustworthy file list means no way to reason about the hot fold either, so the latency gate
        // fires for the same reason everything else does: err toward measuring (`core_direct` too).
        None => (
            g.names.clone(),
            g.names.clone(),
            g.names.clone(),
            g.names.clone(),
            g.names.clone(),
            (true, true),
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
            let mut global: BTreeSet<String> = files
                .iter()
                .filter(|f| escalates(f, cwd) && !commented.contains(f))
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
            // The lane: what the changed files' readers need, plus the gate crate for a comment-only
            // edit of a global file ([`comment_only_global`] says why it is not nothing) or for a
            // dependency edit (the root manifest and the lockfile are text its gates read).
            let mut lane = lane_crates_for(files);
            if !commented.is_empty() || dep_edit {
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
            // The latency pair's two inputs, the crates that OWN a changed file that can reach the measured
            // binary ([`latency_compiled`]: every file but a test-only one, the harness's own files
            // excepted). `lat_changed` is what `core` walks (it also holds the `dep_consumers`
            // members, as `changed` does); `lat_owners` is what `core_direct` asks (no consumer
            // member: a moved workspace dependency is a transitive case). One classification, two
            // sets, so the push rule and the pull-request rule cannot disagree about a file.
            let mut lat_changed: BTreeSet<String> = BTreeSet::new();
            let mut lat_owners: BTreeSet<String> = BTreeSet::new();
            let mut feature_owners: BTreeSet<String> = BTreeSet::new();
            // The owners among them whose dev enablers include `vike-ops`: its gates compile such a module,
            // so the edit is one the gates' links must be asked about.
            let mut ops_enabled_owners: BTreeSet<String> = BTreeSet::new();
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
                if !test_only {
                    match dir.and_then(|d| graph::feature_test_module(d, &rel)) {
                        Some(feature) => {
                            let (dev, normal) = g.feature_enablers(&name, &feature);
                            if normal.is_empty() {
                                if dev.contains(ops_gates::OPS_CRATE) {
                                    ops_enabled_owners.insert(name.clone());
                                }
                                enablers.extend(dev);
                                feature_owners.insert(name.clone());
                            } else {
                                linkable.insert(name.clone());
                            }
                        }
                        None => {
                            linkable.insert(name.clone());
                        }
                    }
                }
                // A build script may declare modules (`#[path = "build/x.rs"] mod x;`), so `build/`
                // is part of it: an edit there is a BUILD SCRIPT edit.
                if rel == "Cargo.toml" || rel == "build.rs" || rel.starts_with("build/") {
                    manifest.insert(name.clone());
                }
                if latency_compiled(f, test_only) {
                    lat_changed.insert(name.clone());
                    lat_owners.insert(name.clone());
                }
                changed.insert(name);
            }
            // The members that link a moved package: tested like a changed crate, and a MANIFEST
            // edit for the structural suites — a dependency or feature edit is what they look for.
            for member in dep_consumers.iter().flatten() {
                linkable.insert(member.clone());
                manifest.insert(member.clone());
                lat_changed.insert(member.clone());
                changed.insert(member.clone());
            }
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
            // `xtask/tests/ci_plan_gate/latency_test_edits.rs` pins both halves.
            // A dependency-only root edit is judged through its consumers (`changed` above), not by
            // the names `Cargo.toml` and `Cargo.lock`, which are latency inputs for every OTHER edit.
            let latency_files: Vec<String> = files
                .iter()
                .filter(|f| !commented.contains(f))
                .filter(|f| !(dep_edit && (*f == "Cargo.toml" || *f == "Cargo.lock")))
                .cloned()
                .collect();
            let core = latency_affected(&latency_files, &lat_changed, &g.radj);
            // The narrower, pull-request-only question (`ci.yml`'s `latency` job ANDs it with `core`).
            // Over the RAW file list, not `latency_files`: a dependency-only root edit is judged
            // through its consumers above, but its `Cargo.lock` is still a direct input here.
            let core_direct = latency_direct(files, &lat_owners);
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
            let docs = !global.is_empty()
                || docs_affected(&linkable, &g.names)
                || files.iter().any(|f| tables::readers::DOCS_JOB_INPUTS.contains(&f.as_str()));
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
            ops_everything = !global.is_empty() || dep_reaches_ops;
            reach.extend(enablers);
            // What a trigger the suite only COMPILES as a dependency is matched against
            // (`tables::suite_rules::DEPENDENCY_ONLY_TRIGGERS`): the crates with an edit that can LINK, which is
            // `linkable` — a test-only edit to such a crate changes nothing the suite builds — plus
            // the crates of a `test-support` module edit, which a dev enabler's test build links.
            let compiled: BTreeSet<String> = if global.is_empty() {
                linkable.union(&feature_owners).cloned().collect()
            } else {
                g.names.clone()
            };
            // The closure of every edit something can BUILD: the graph's reach, the crates of a linkable
            // or `test-support` edit (`compiled`), and the gate crates. It is `affected` MINUS the crates
            // whose only changed files are `tests/`/`benches/`/`examples/` sources or `cfg(test)` modules —
            // what the closure suites are matched against (`tables::suite_rules::CLOSURE_SUITES`), because
            // no arm of theirs compiles such a file except through the crates
            // `tables::suite_rules::CLOSURE_SUITE_TEST_BUILDS` names, which are matched on `affected`.
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
            // The set a feature suite OUTSIDE [`tables::suite_rules::CLOSURE_SUITES`] is matched against: the crates
            // that OWN a changed file, with no reverse-dep walk — and EVERY crate when a global file
            // changed, for the same reason `affected` is every crate then. See `tables::suite_rules::CLOSURE_SUITES`.
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
            (
                affected,
                closure,
                direct,
                structural,
                compiled,
                (core, core_direct),
                docs,
                lane,
                suite_keys_for(files),
            )
        }
    };
    let (core, core_direct) = lat;

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
    let ordered: Vec<String> =
        roster.iter().filter(|c| affected.contains(*c) || lane.contains(*c)).cloned().collect();
    // Which of `vike-ops`'s gates run (`ops_gates`' module doc is the rule list). `None` = all of them:
    // no file list, a crate it links changed, or any doubt inside `ops_gates::select`.
    let ops_gates = match &files {
        Some(files) if !ops_everything && ordered.iter().any(|c| c == ops_gates::OPS_CRATE) => {
            narrowed_ops_gates(files, base.as_deref(), cwd, &g, triggers, &graph_reach)
        }
        _ => None,
    };
    // A suite fires when a crate it compiles is affected — or when a changed file's only reader is
    // a test that suite alone compiles (`tables::readers::SUITE_INPUT_READERS`). Declaration order either
    // way: this list is the matrix order.
    let affected_keys: Vec<String> = tables::feature_suites::FEATURE_SUITES
        .iter()
        .filter(|(k, triggers)| {
            // A closure suite is matched against the closure of what something can build (`closure`),
            // so a TEST-ONLY edit of a trigger fires nothing — except the triggers whose own test
            // targets its arm builds (`tables::suite_rules::CLOSURE_SUITE_TEST_BUILDS`), which are
            // matched on `affected`, where a crate that owns any changed file stands.
            let test_built: &[&str] = tables::suite_rules::CLOSURE_SUITE_TEST_BUILDS
                .iter()
                .find(|(suite, _)| suite == k)
                .map_or(&[][..], |(_, crates)| *crates);
            let seed = if tables::suite_rules::CLOSURE_SUITES.contains(k) {
                &closure
            } else if tables::suite_rules::MANIFEST_CLOSURE_SUITES.contains(k) {
                &structural
            } else {
                &direct
            };
            // A trigger this suite only COMPILES as a dependency is matched against the crates with
            // a linkable edit, not every crate that owns a changed file.
            let dep_only: &[&str] = tables::suite_rules::DEPENDENCY_ONLY_TRIGGERS
                .iter()
                .find(|(suite, _)| suite == k)
                .map_or(&[][..], |(_, crates)| *crates);
            triggers.iter().any(|t| {
                if dep_only.contains(t) {
                    compiled.contains(*t)
                } else {
                    seed.contains(*t) || (test_built.contains(t) && affected.contains(*t))
                }
            }) || forced.contains(k)
        })
        .map(|(k, _)| (*k).to_string())
        .collect();
    let suites = pack_suites(&affected_keys);

    Ok(Plan {
        any: !ordered.is_empty(),
        hist: super::selection::hist_affected(&direct),
        core,
        core_direct,
        app: affected.contains(tables::roster::APP_CHECK_CRATE),
        // The DIRECT set and the raw file list, never `affected` ([`lightgbm_affected`] says why).
        lightgbm: lightgbm_affected(&direct, files.as_deref().unwrap_or_default()),
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

/// [`graph::load_graph`] plus the one sanity check that must be LOUD rather than a silent disarm.
///
/// [`tables::roster::LIGHTGBM_CRATES`] is a hand-written set of crate NAMES, and the job it gates exists
/// precisely because a suite that quietly runs nothing looks exactly like a suite that passes. Rename
/// or delete one of those crates and the intersection would simply stop matching: the job would never
/// fire again, no output would change, and nothing would be red. LATENCY does not carry this check
/// because its set gates a job that reruns on nearly every PR anyway; this one is narrow enough to
/// vanish. HIST is narrow now too (a direct edit of one of four crates), and is held by a test rather
/// than by a refusal here: `xtask/tests/ci_plan_gate/lane_directness.rs`'s
/// `every_hist_crate_is_a_workspace_member`.
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
    Ok(g)
}
