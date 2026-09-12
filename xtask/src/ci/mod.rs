//! Compute the CI test/clippy plan for a push or a PR — the ONE implementation of it.
//!
//! # The output contract
//!
//! [`Plan::render`] emits, in this order, the eight `key=value` lines
//! `.github/workflows/ci.yml`'s `plan` job turns into job outputs:
//!
//!   * `any`      — "true" if there is anything to test this run (gates the `test` job)
//!   * `crates`   — a ready-to-use `-p a -p b …` fragment of the affected CI crates
//!   * `suites`   — JSON array of feature-suite MATRIX LEGS: each entry is one suite key, or
//!     several space-separated keys packed into one job by [`pack_suites`] over
//!     [`tables::SUITE_GROUPS`] (ci.yml passes the entry to `scripts/ci_feature_suite.sh`
//!     unquoted, so the shell hands the script one key per argument)
//!   * `hist`     — the DataFusion hist job
//!   * `core`     — the the latency box latency gate. NOT "vike-core is affected" — see [`latency_affected`]
//!   * `app`      — the vike-desktop compile gate
//!   * `lightgbm` — the the CI box LightGBM job
//!   * `docs`     — the API-reference build + publish job ([`docs_affected`])
//!
//! ⚠ This output shape is a CONTRACT with `.github/workflows/{ci,release}.yml`, not an internal
//! detail: those files consume the lines by name and nothing else re-derives them. It was
//! developed to be byte-identical to the Python planner it replaced — measured over 58 hermetic
//! scenarios and 6 real-repository plans on Linux, stdout AND stderr, diagnostic included — which is
//! what made the switch a one-line diff in each workflow rather than a rewrite. That equivalence was
//! the migration's evidence and is now history: this file is the authority, and
//! `crates/vike-ops/tests/ci_plan_gate.rs` holds what survived it.
//!
//! # Selection
//!
//! The same rule for a PR and for a push to main (a push just diffs against the pre-push SHA instead
//! of the PR's merge base):
//!
//!   * only the crates a changed file OWNS, plus every crate that (transitively) depends on them
//!     through a NORMAL dep — so a break downstream is never missed — intersected with the CI roster.
//!     Dev-dependents are added ONE hop (see [`graph::affected_from`]).
//!   * ESCALATIONS to the FULL roster, so narrowing can never silently under-test: a workspace-global
//!     file ([`tables::GLOBAL_PREFIXES`]), EXCEPT a purely additive `Cargo.lock`-only change; an
//!     unresolvable diff base; or `CI_FULL=1`, the manual escape hatch.
//!   * COMPANIONS: a selected crate whose tests SPAWN another crate's shipped binary pulls that
//!     crate into the test LANE ([`binary_companions_for`]). A companion rule over the affected
//!     SET, not over the file list — a spawn is not a dependency edge, so the closure above cannot
//!     reach it and cargo cannot either.
//!
//! The affected crates run in ONE `test` job (one shared warm cache), not sharded — sharding across
//! N jobs with a single cache key would leave all-but-one shard cold every run, because GitHub cache
//! keys are write-once. Parallelism inside the job comes from cargo + nextest using every core.

pub mod git;
pub mod graph;
pub mod tables;

use std::collections::BTreeSet;
use std::path::Path;

pub use git::Env;
use graph::{Graph, Rdeps, affected_from};

/// The prefix on the one diagnostic line this tool writes to stderr.
///
/// It read `ci_affected:` for as long as the Python planner existed, so the two could be compared
/// verbatim. That file is gone, and a prefix naming a deleted script sends the next reader of a CI
/// log looking for something that is not there — so it names the thing it IS.
pub const DIAG_PREFIX: &str = "ci-plan:";

/// The computed plan. `diagnostic` is stderr, everything else is stdout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub any: bool,
    /// The affected roster crates, in roster order.
    pub ordered: Vec<String>,
    /// Feature-suite matrix legs (a key, or several space-joined keys packed by [`pack_suites`]),
    /// ordered by each leg's first key's [`tables::FEATURE_SUITES`] declaration position.
    pub suites: Vec<String>,
    pub hist: bool,
    pub core: bool,
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
    pub fn render(&self) -> String {
        let crates: Vec<String> = self.ordered.iter().map(|c| format!("-p {c}")).collect();
        let suites: Vec<String> = self.suites.iter().map(|s| format!("\"{s}\"")).collect();
        let b = |v: bool| if v { "true" } else { "false" };
        format!(
            "any={}\ncrates={}\nsuites=[{}]\nhist={}\ncore={}\napp={}\nlightgbm={}\ndocs={}\n",
            b(self.any),
            crates.join(" "),
            suites.join(", "),
            b(self.hist),
            b(self.core),
            b(self.app),
            b(self.lightgbm),
            b(self.docs),
        )
    }
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

/// The derived roster: every workspace member EXCEPT [`tables::EXCLUDE_FROM_CI`], sorted.
///
/// Deriving it rather than writing it down is what makes a NEW crate join the merge gate the moment
/// it joins `[workspace].members`, with no CI edit and no list to update.
pub fn ci_crates(names: &BTreeSet<String>) -> Vec<String> {
    let excluded = set(tables::EXCLUDE_FROM_CI);
    names.difference(&excluded).cloned().collect()
}

/// The roster the PUBLISHED API reference documents — the CI roster, and that is a DECISION.
///
/// It is spelled as a call to [`ci_crates`] rather than as a second table, so the two answers
/// cannot drift and a new crate joins the published reference the moment it joins
/// `[workspace].members` — the property `crates/vike-ops/tests/local_gate_mirrors_ci.rs`'s header
/// records four rotted hand copies for. What the equality BUYS, stated so a future divergence has
/// to argue against it: nothing reaches a public documentation page before the merge gate has
/// compiled and linted it, because the doc build cannot name a crate the gate does not.
///
/// What that leaves out is exactly [`tables::EXCLUDE_FROM_CI`], and each exclusion is also correct
/// for a reader of an API reference: `vike-desktop` is the egui/wgpu GUI BINARY — a composition root
/// with no library API to call — `vike-backfill` is a set of feature-gated collector binaries whose
/// default build documents almost nothing, and `ibapi` is a vendored copy of an upstream crate that
/// is already published on docs.rs by its own author. A crate that genuinely belongs in the
/// reference but not in the merge gate would need a table of its own; there is none today, and
/// inventing one before a member needs it is how the second roster starts.
pub fn doc_crates(names: &BTreeSet<String>) -> Vec<String> {
    ci_crates(names)
}

/// Does this change move the PUBLISHED API reference?
///
/// ⚠ Computed from the crates that OWN a changed file, never from the reverse-dep closure, and the
/// difference is the whole reason this is its own output rather than a reuse of `any`. rustdoc
/// renders crate X's items and doc comments out of crate X's own sources: a change in a crate that
/// DEPENDS on X cannot alter one byte of X's pages, so the closure — which is the right answer for
/// "could this break a build" — is the wrong answer for "did the publication change" and would fire
/// this job on nearly every pull request. `affected` additionally carries the whole-tree gate crates
/// ([`gate_crates_for`]), so a markdown-only typo selects `vike-ops` and an `any`-gated docs job
/// would rebuild the entire reference for it.
///
/// `linkable` rather than `changed`, for the same reason [`compute`] uses it for the dep hop: a
/// `tests/`, `benches/` or `examples/` source is compiled into its own binary and rustdoc never
/// reads it, so it cannot move a documented page either.
pub fn docs_affected(linkable: &BTreeSet<String>, names: &BTreeSet<String>) -> bool {
    let roster: BTreeSet<String> = doc_crates(names).into_iter().collect();
    linkable.intersection(&roster).next().is_some()
}

/// The gate-owning crates a change must select REGARDLESS of which crate owns the file.
///
/// Every entry reads an input OUTSIDE any crate's own sources — the whole `.rs` tree, the markdown
/// tree, `deploy/`, a repo-root manifest — so the reverse-dep closure, derived from changed CRATES,
/// structurally cannot select them. A gate that cannot run on its own change class is not a gate:
/// measured, a docs-only PR selected zero crates and not one of the five prose gates ran, and a
/// `server.json`-only PR selected zero crates and did not run the manifest's drift gate.
pub fn gate_crates_for(files: &[String]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    if files.iter().any(|f| f.ends_with(".rs")) {
        out.insert(tables::SETTINGS_GATE_CRATE.to_string());
    }
    let doc_input = |f: &String| {
        tables::DOC_GATE_INPUT_SUFFIXES.iter().any(|s| f.ends_with(s))
            || tables::DOC_GATE_INPUT_PREFIXES.iter().any(|p| f.starts_with(p))
    };
    if files.iter().any(doc_input) {
        out.insert(tables::DOC_GATE_CRATE.to_string());
    }
    // Exact paths rather than a prefix — see `tables::MANIFEST_GATE_INPUTS` for why the repository
    // root cannot be swept wholesale.
    if files.iter().any(|f| tables::MANIFEST_GATE_INPUTS.contains(&f.as_str())) {
        out.insert(tables::MANIFEST_GATE_CRATE.to_string());
    }
    out
}

/// The crates a SELECTED crate's tests SPAWN as BINARIES — force-added to the test LANE.
///
/// Keyed on the affected SET rather than on the changed FILES, which is what separates it from
/// [`gate_crates_for`]: that one answers "which whole-tree gate reads this input", this one answers
/// "what is about to run, and which build ARTIFACT does it need". A driver's own change is what
/// selects it, and the artifact it is missing belongs to a crate no file in the diff mentions.
///
/// [`tables::BINARY_DRIVER_COMPANIONS`] carries the measurement (CI run 34020320299), why the
/// reverse-dep closure structurally cannot find the edge, why selecting the crate is what BUILDS
/// the binary, and the two alternatives that were rejected.
pub fn binary_companions_for(affected: &BTreeSet<String>) -> BTreeSet<String> {
    tables::BINARY_DRIVER_COMPANIONS
        .iter()
        .filter(|(driver, _)| affected.contains(*driver))
        .flat_map(|(_, companions)| companions.iter().map(|c| (*c).to_string()))
        .collect()
}

/// `(name, version)` pairs from a `Cargo.lock`'s `[[package]]` blocks.
fn lock_pkgs(text: &str) -> BTreeSet<(String, String)> {
    let mut pkgs = BTreeSet::new();
    let mut name: Option<String> = None;
    for line in text.lines() {
        let s = line.trim();
        if s == "[[package]]" {
            name = None;
        } else if let Some(rest) = s.strip_prefix("name = ") {
            name = Some(rest.trim().trim_matches('"').to_string());
        } else if let Some(rest) = s.strip_prefix("version = ")
            && let Some(n) = &name
        {
            pkgs.insert((n.clone(), rest.trim().trim_matches('"').to_string()));
        }
    }
    pkgs
}

/// True iff every `(name, version)` in the OLD lock is still present in the NEW one — i.e. the change
/// only ADDED packages and moved or removed nothing.
///
/// Such a diff cannot change any already-resolved crate's version, so crates unrelated to the
/// accompanying source change are unaffected and the full-matrix escalation is not needed. An empty
/// old lock (parse failure, no base) is `false`, so the answer stays conservative.
pub fn lock_additive_only(old_text: &str, new_text: &str) -> bool {
    let old = lock_pkgs(old_text);
    !old.is_empty() && old.is_subset(&lock_pkgs(new_text))
}

/// Disk/git wrapper over [`lock_additive_only`]: the base commit's `Cargo.lock` against HEAD's on
/// disk. Any error (no base ref, unreadable file) is `false` — the conservative full matrix.
fn cargo_lock_additive_only(base: &str, cwd: &Path) -> bool {
    let Some(old) = git::show_cargo_lock(base, cwd) else { return false };
    let Ok(new) = std::fs::read_to_string(cwd.join("Cargo.lock")) else { return false };
    lock_additive_only(&old, &new)
}

/// True iff this change can move the p99 the the latency box latency gate measures.
///
/// ⚠ Deliberately NOT `"vike-core" in affected`, which is what it used to be. That expression READS
/// as "could this touch the hot fold?" and it does not, because `affected` is widened by two
/// mechanisms that have nothing to do with the measured binary: one hop of DEV-dependents (vike-core
/// dev-depends upward on vike-mm/vike-strategy/vike-script, none of which is in its compiled
/// library), and ESCALATION (any workspace-global file made `affected` every crate, so `deny.toml`,
/// the `justfile` or `.github/CODEOWNERS` each bought an ~11-minute microsecond measurement on
/// reserved cores).
///
/// So the question is asked directly instead: does the change touch a file that compiles INTO the
/// measured binary, or the harness that measures it? The crate half walks NORMAL edges only (an
/// empty `radj_dev` is passed), which drops the dev-only false positives while keeping every real
/// one — a `vike-model` change still reaches `vike-core` through `radj`. It also self-maintains
/// DOWNWARD: a crate inserted below any [`tables::LATENCY_CRATES`] member fires the gate with no
/// edit to that table.
pub fn latency_affected(files: &[String], changed_crates: &BTreeSet<String>, radj: &Rdeps) -> bool {
    let is_latency_input = |f: &String| {
        f.as_str() == "Cargo.toml"
            || tables::LATENCY_GLOBAL_PREFIXES.iter().any(|p| f.starts_with(p))
    };
    if files.iter().any(is_latency_input) {
        return true;
    }
    let latency = set(tables::LATENCY_CRATES);
    affected_from(changed_crates, radj, &Rdeps::new()).intersection(&latency).next().is_some()
}

/// Pack the affected suite keys into `features` matrix legs.
///
/// Input: the affected [`tables::FEATURE_SUITES`] keys, in declaration order. Output: one entry
/// per matrix leg — a key on its own, or every affected member of one [`tables::SUITE_GROUPS`]
/// set joined by single spaces (in input order), emitted at the position of the set's FIRST
/// affected member. A set with one affected member emits that bare key, so a narrow PR's plan is
/// byte-identical to the pre-packing one; packing can only ever MERGE jobs, never add work to one.
///
/// The space join is the contract with ci.yml's `features` job: `${{ matrix.suite }}` is passed
/// to `scripts/ci_feature_suite.sh` unquoted, the shell word-splits it, and the script runs each
/// argument's `case` arm in order — the same invocations the keys ran as separate jobs.
pub fn pack_suites(keys: &[String]) -> Vec<String> {
    let mut emitted: Vec<usize> = Vec::new();
    let mut out: Vec<String> = Vec::new();
    for k in keys {
        match tables::SUITE_GROUPS.iter().position(|g| g.contains(&k.as_str())) {
            None => out.push(k.clone()),
            Some(gi) => {
                if emitted.contains(&gi) {
                    continue;
                }
                emitted.push(gi);
                let leg: Vec<&str> = keys
                    .iter()
                    .map(String::as_str)
                    .filter(|key| tables::SUITE_GROUPS[gi].contains(key))
                    .collect();
                out.push(leg.join(" "));
            }
        }
    }
    out
}

/// True iff the the CI box LightGBM job should run.
///
/// Deliberately the same shape as the `hist` trigger — an intersection with the reverse-dep closure —
/// and NOT [`latency_affected`]'s narrower question. This job asks "could this change break the
/// suites that drive the trainer?", which the closure genuinely answers; the latency gate asks "did
/// the MEASURED binary move?", which it does not.
pub fn lightgbm_affected(affected: &BTreeSet<String>) -> bool {
    affected.intersection(&set(tables::LIGHTGBM_CRATES)).next().is_some()
}

/// Compute the plan. `env` is the caller's environment and `cwd` the repository root — both
/// PARAMETERS rather than `std::env`/CWD reads, which is what lets
/// `crates/vike-ops/tests/ci_plan_gate.rs` drive the whole ladder over a planted git topology
/// in-process, with no ambient CI variable able to change what a scenario measures.
pub fn compute(env: &Env, cwd: &Path) -> Result<Plan, String> {
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

    let (affected, core, docs) = match &files {
        // CI_FULL is the manual escape hatch; `files is None` = an unresolvable diff base. No
        // trustworthy file list means no way to reason about the hot fold either, so the latency gate
        // fires for the same reason everything else does: err toward measuring.
        None => (g.names.clone(), true, true),
        Some(files) => {
            let is_global = |f: &String| {
                f.as_str() == "Cargo.toml"
                    || tables::GLOBAL_PREFIXES.iter().any(|p| f.starts_with(p))
            };
            let mut global: BTreeSet<String> =
                files.iter().filter(|f| is_global(f)).cloned().collect();
            // EXCEPT a Cargo.lock-ONLY global change that is purely additive: it cannot move a
            // resolved version, so the narrow selection from the accompanying source changes holds.
            let lock_only = global.len() == 1 && global.contains("Cargo.lock");
            if lock_only && cargo_lock_additive_only(base.as_deref().unwrap_or_default(), cwd) {
                global.clear();
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
            for f in files {
                let Some((name, rel)) = graph::owner_of(f, &g.dirs, cwd) else { continue };
                if !graph::is_test_target_source(&rel) {
                    linkable.insert(name.clone());
                }
                changed.insert(name);
            }
            // Computed from `files`/`changed` directly, NEVER from `affected` — the whole point is
            // that the escalation below must not be able to buy a microsecond measurement.
            //
            // ⚠ `changed`, deliberately NOT `linkable`: the gate's own harness
            // (`crates/vike-core/tests/runtime_latency.rs`) IS a test-target source, so narrowing
            // here would let a change to how the p99 is MEASURED ship without re-measuring it. This
            // rule may make the latency gate fire no less often than before — that is a property,
            // and `crates/vike-ops/tests/ci_plan_gate.rs` pins it.
            let core = latency_affected(files, &changed, &g.radj);
            // Same two inputs, a different question — see [`docs_affected`]. A GLOBAL file forces
            // it for the reason it forces everything else: the root manifest, the toolchain and the
            // lockfile all reach every crate's rendered pages, and a file list this run cannot
            // reason about is not evidence that the reference is unchanged.
            let docs = !global.is_empty() || docs_affected(&linkable, &g.names);
            let affected = if global.is_empty() {
                // The dev hop rides `linkable` too: a crate whose TESTS use X is affected when X's
                // LIB moves, and a test-target source is not X's lib.
                let mut a = affected_from(&linkable, &g.radj, &g.radj_dev);
                a.extend(changed.iter().cloned());
                a.extend(gate_crates_for(files));
                a
            } else {
                g.names.clone()
            };
            (affected, core, docs)
        }
    };

    // `affected` is the full workspace-crate set touched (used for the feature/hist triggers);
    // `ordered` is the subset the `test` job runs — plus two force-adds that join the LANE without
    // joining `affected`: the store-kind gate's crate (see `tables::STORE_KIND_GATE_CRATE`) and the
    // binaries a selected crate's tests SPAWN (`tables::BINARY_DRIVER_COMPANIONS`). Both are
    // deliberately lane-only — feeding either into `affected` would fire the feature/hist matrix
    // those crates trigger, to buy a lane and a build artifact respectively.
    let rs_touched = files.as_ref().is_none_or(|f| f.iter().any(|p| p.ends_with(".rs")));
    let companions = binary_companions_for(&affected);
    let ordered: Vec<String> = roster
        .into_iter()
        .filter(|c| {
            affected.contains(c)
                || companions.contains(c)
                || (rs_touched && c == tables::STORE_KIND_GATE_CRATE)
        })
        .collect();
    let affected_keys: Vec<String> = tables::FEATURE_SUITES
        .iter()
        .filter(|(_, triggers)| triggers.iter().any(|t| affected.contains(*t)))
        .map(|(k, _)| (*k).to_string())
        .collect();
    let suites = pack_suites(&affected_keys);

    Ok(Plan {
        any: !ordered.is_empty(),
        hist: affected.intersection(&set(tables::HIST_CRATES)).next().is_some(),
        core,
        app: affected.contains(tables::APP_CHECK_CRATE),
        lightgbm: lightgbm_affected(&affected),
        docs,
        ordered,
        suites,
        diagnostic,
    })
}

/// [`graph::load_graph`] plus the one sanity check that must be LOUD rather than a silent disarm.
///
/// [`tables::LIGHTGBM_CRATES`] is a hand-written set of crate NAMES, and the job it gates exists
/// precisely because a suite that quietly runs nothing looks exactly like a suite that passes. Rename
/// or delete one of those crates and the intersection would simply stop matching: the job would never
/// fire again, no output would change, and nothing would be red. HIST/LATENCY do not carry this check
/// because their sets gate jobs that rerun on nearly every PR anyway; this one is narrow enough to
/// vanish.
fn load_graph_checked(cwd: &Path) -> Result<Graph, String> {
    let g = graph::load_graph(cwd)?;
    let unknown: Vec<&str> =
        tables::LIGHTGBM_CRATES.iter().copied().filter(|c| !g.names.contains(*c)).collect();
    if !unknown.is_empty() {
        return Err(format!(
            "LIGHTGBM_CRATES names {unknown:?}, which are not workspace members. The the CI box LightGBM \
             job would silently never fire again. Update xtask::ci::tables::LIGHTGBM_CRATES (and the \
             suites in scripts/ci_lightgbm_suite.sh) to the new names."
        ));
    }
    Ok(g)
}

#[cfg(test)]
mod docs_roster_tests {
    use super::*;

    fn names() -> BTreeSet<String> {
        set(&["vike-model", "vike-core", "vike-desktop", "vike-backfill", "ibapi", "xtask"])
    }

    /// The published reference documents the CI roster — asserted as an EQUALITY between the two
    /// derivations rather than against a written-down list, so this test still holds the day a
    /// crate is added and cannot be satisfied by pasting one in.
    #[test]
    fn the_documented_roster_is_the_ci_roster() {
        let n = names();
        assert_eq!(doc_crates(&n), ci_crates(&n));
        assert!(doc_crates(&n).contains(&"vike-model".to_string()));
        // ...and the exclusions really are excluded, so an equality that had degenerated to "every
        // member" would fail here rather than read green.
        for excluded in ["vike-desktop", "vike-backfill", "ibapi"] {
            assert!(
                !doc_crates(&n).contains(&excluded.to_string()),
                "{excluded} is in EXCLUDE_FROM_CI and must not reach the published reference"
            );
        }
    }

    /// The trigger fires on a documented crate's own source and on nothing else — the property that
    /// separates it from `any` and keeps a markdown typo from rebuilding the whole reference.
    #[test]
    fn the_docs_trigger_reads_the_owning_crate_not_the_closure() {
        let n = names();
        assert!(docs_affected(&set(&["vike-model"]), &n), "a documented crate's own source");
        assert!(
            !docs_affected(&set(&["vike-desktop"]), &n),
            "vike-desktop is not documented, so a change confined to it cannot move a published page"
        );
        assert!(
            !docs_affected(&set(&[]), &n),
            "a change owning no crate at all — a markdown typo — must not fire the docs job"
        );
    }

    /// The eighth line is EMITTED. `.github/workflows/ci.yml` reads `docs` off `$GITHUB_OUTPUT` by
    /// name, and an output the plan never prints is permanently empty and silently falsy — the
    /// exact defect the `shards` output sat in for months (see that file's `outputs:` block).
    #[test]
    fn render_emits_the_docs_key_in_both_states() {
        let mut plan = Plan {
            any: true,
            ordered: vec!["vike-model".to_string()],
            suites: vec![],
            hist: false,
            core: false,
            app: false,
            lightgbm: false,
            docs: true,
            diagnostic: String::new(),
        };
        assert!(plan.render().contains("\ndocs=true\n"), "{}", plan.render());
        plan.docs = false;
        assert!(plan.render().contains("\ndocs=false\n"), "{}", plan.render());
    }
}
