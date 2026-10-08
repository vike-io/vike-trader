//! Compute the CI test/clippy plan for a push or a PR — the ONE implementation of it.
//!
//! # The output contract
//!
//! [`plan::Plan::render`] emits, in this order, the `key=value` lines `.github/workflows/ci.yml`'s `plan`
//! job turns into job outputs (no count here: `crates/vike-ops/tests/release/api_docs_gate.rs` reads the set
//! off `render`'s format string, and a count written beside it rots):
//!
//!   * `any`      — "true" if there is anything to test this run (gates the `test` job)
//!   * `crates`   — a ready-to-use `-p a -p b …` fragment of the affected CI crates
//!   * `roster`   — the same fragment over the WHOLE derived roster ([`roster::ci_crates`]), whatever the
//!     change touched: what the `test` job BUILDS, documents and lints. See [`plan::Plan::roster`]
//!   * `tests`    — a nextest filterset selecting exactly the `crates` set's tests out of that
//!     build ([`roster::nextest_filter`]): what the `test` job RUNS. Empty when `crates` is empty
//!     (one crate's term may be narrowed to some of its test binaries: [`ops_gates`])
//!   * `suites`   — JSON array of feature-suite MATRIX LEGS: each entry is one suite key, or
//!     several space-separated keys packed into one job by [`selection::pack_suites`] over
//!     [`tables::suite_rules::SUITE_GROUPS`] (ci.yml passes the entry to `scripts/ci_feature_suite.sh`
//!     unquoted, so the shell hands the script one key per argument)
//!   * `hist`     — the DataFusion hist job
//!   * `core`     — the the latency box latency gate. NOT "vike-core is affected" — see [`selection::latency_affected`]
//!   * `app`      — the vike-desktop compile gate
//!   * `lightgbm` — the the CI box LightGBM job
//!   * `docs`     — the API-reference build + publish job ([`roster::docs_affected`])
//!
//! ⚠ This output shape is a CONTRACT with `.github/workflows/{ci,release}.yml`, not an internal
//! detail: those files consume the lines by name and nothing else re-derives them. It was
//! developed to be byte-identical to the Python planner it replaced — measured over 58 hermetic
//! scenarios and 6 real-repository plans on Linux, stdout AND stderr, diagnostic included — which is
//! what made the switch a one-line diff in each workflow rather than a rewrite. That equivalence was
//! the migration's evidence and is now history: this file is the authority, and
//! `xtask/tests/ci_plan_gate.rs` holds what survived it.
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
//!     file ([`selection::escalates`]: [`tables::escalation::GLOBAL_PREFIXES`] minus the still-present files
//!     [`tables::escalation::is_global_exempt`] exempts), EXCEPT a purely additive `Cargo.lock`-only change;
//!     a global file whose WHOLE change is whole-line `#` comments and blank lines is NOT a global edit
//!     ([`selection::comment_only_global`]: it plans the gate crate into the lane, no roster, suite or latency run);
//!     a root `Cargo.toml` change confined to `[workspace.dependencies]` (with its `Cargo.lock`) plans the members
//!     whose resolved closure holds a moved package instead ([`workspace_deps::consumers`]);
//!     an unresolvable diff base; or `CI_FULL=1`, the manual escape hatch.
//!   * INPUT READERS: a changed file that owns no crate but is READ by some crate's tests pulls
//!     that crate into the test LANE ([`selection::lane_crates_for`]) — never into `affected`, so it fires no
//!     feature suite. That is what an exempted `scripts/` path selects instead of the full roster.
//!     A reader that only a FEATURE SUITE compiles fires that suite instead ([`selection::suite_keys_for`]).
//!   * PER-GATE TRIGGERS: `vike-ops` is a CRATE of one test binary per gate, and the `test` job's filter may
//!     name only some of them ([`ops_gates::select`], rows in [`tables::gate_triggers::GATE_TRIGGERS`]). The
//!     crate stays in `crates=` and in the roster build either way; only `tests=` narrows, and only when
//!     the change is one the rows can judge (every doubt runs every gate); an edit of a crate also selects the
//!     gates whose code links it ([`tables::gate_triggers::GateTrigger::links`]).
//!   * SPAWNED BINARIES are not a selection rule any more. A crate whose tests spawn another crate's
//!     shipped binary needs that binary BUILT, and the `test` job builds the whole roster
//!     ([`plan::Plan::roster`]) whatever it runs; [`tables::roster::BINARY_DRIVER_COMPANIONS`] records the edge
//!     cargo cannot see, and argues (with the measurement) why its old force-add into the lane went.
//!
//! The affected crates run in ONE `test` job (one shared warm cache), not sharded — sharding across
//! N jobs with a single cache key would leave all-but-one shard cold every run, because GitHub cache
//! keys are write-once. Parallelism inside the job comes from cargo + nextest using every core.

pub mod git;
pub mod graph;
pub mod lockfile;
pub mod ops_gates;
pub mod plan;
pub mod roster;
pub mod selection;
pub mod tables;
pub mod workspace_deps;

use std::collections::BTreeSet;

pub use git::Env;

/// The prefix on the one diagnostic line this tool writes to stderr.
///
/// It read `ci_affected:` for as long as the Python planner existed, so the two could be compared
/// verbatim. That file is gone, and a prefix naming a deleted script sends the next reader of a CI
/// log looking for something that is not there — so it names the thing it IS.
pub const DIAG_PREFIX: &str = "ci-plan:";

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

#[cfg(test)]
mod docs_roster_tests;
