//! The crate lists: the derived CI roster, the documented roster, and their cargo/nextest spellings.

use std::collections::BTreeSet;

#[cfg(doc)]
use super::plan::{Plan, compute};
#[cfg(doc)]
use super::selection::gate_crates_for;
use super::{set, tables};

/// `-p a -p b …` — the ONE spelling of a crate list as cargo arguments, shared by the plan's
/// `crates`/`roster` lines and the `crates`/`doc-crates` commands. Empty for an empty list.
pub fn package_args(crates: &[String]) -> String {
    crates.iter().map(|c| format!("-p {c}")).collect::<Vec<_>>().join(" ")
}

/// The nextest filterset that selects exactly `crates`' tests: `package(=a) | package(=b) | …`.
/// Empty for an empty list — there is no filterset that means "nothing", and an empty `-E` is a
/// parse error, so a consumer that runs on an empty plan fails rather than testing nothing.
///
/// ⚠ The `=` is load-bearing. `package()`'s DEFAULT matcher is a glob: MEASURED on nextest 0.9.143,
/// `package(vike-lo)` matches no package while `package(vike-log)` matches exactly one. An exact
/// matcher says what is meant whatever a future nextest picks as the default — a `contains` default
/// would make `package(vike-studio)` also run `vike-studio-core`, and every name here is a prefix
/// of a sibling somewhere in this workspace (`vike-data`/`vike-data-manager`,
/// `vike-tradehub`/`vike-tradehub-client`, …).
///
/// What nextest does with the two ways this string could be wrong, MEASURED on the same version:
///
///   * a name that is not a workspace package — a typo, a renamed crate — fails the whole run with
///     `no packages matched this` (exit 94), even beside names that do match. Not a silent drop.
///   * a name that IS a workspace package but is not in the BUILT `-p` set selects nothing from
///     it, SILENTLY (exit 0). That is why the plan builds [`Plan::roster`], of which `ordered` is a
///     filtered subset by construction (`compute`), and why `xtask/tests/ci_plan_gate.rs`
///     holds every name here to that roster on every plan shape it plants.
///
/// `ops_gates` narrows ONE crate, `vike-ops` ([`super::ops_gates::OPS_CRATE`]): `Some(gates)` replaces its
/// term by `(package(=vike-ops) & (binary(=a) | binary(=b) | …))`, the package term intersected with the
/// gates' test binaries (`binary()` takes the `[[test]] name`); `None` leaves it `package(=vike-ops)`. Every
/// other crate is spelled exactly as before. The parentheses keep the intersection
/// unambiguous inside the longer ` | ` chain, whatever the operator precedence is.
/// A narrowed crate with no gate left contributes no term at all, and a filter that would then be EMPTY
/// falls back to the whole package, because an empty `-E` is a parse error and a plan that names a crate
/// must run something. A gate name outside `[A-Za-z0-9_-]` cannot be a cargo target name, so one is
/// refused the same way (the whole package) rather than spliced into a filterset.
pub fn nextest_filter(crates: &[String], ops_gates: Option<&[String]>) -> String {
    let legal = |g: &String| {
        !g.is_empty() && g.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    };
    let narrowed = ops_gates.filter(|gates| gates.iter().all(legal));
    let term = |c: &String| match narrowed {
        Some(gates) if c == super::ops_gates::OPS_CRATE => {
            let binaries =
                gates.iter().map(|g| format!("binary(={g})")).collect::<Vec<_>>().join(" | ");
            (!gates.is_empty()).then(|| format!("(package(={c}) & ({binaries}))"))
        }
        _ => Some(format!("package(={c})")),
    };
    let terms: Vec<String> = crates.iter().filter_map(term).collect();
    if terms.is_empty() && !crates.is_empty() {
        return nextest_filter(crates, None);
    }
    terms.join(" | ")
}

/// The derived roster: every workspace member EXCEPT [`tables::roster::EXCLUDE_FROM_CI`], sorted.
///
/// Deriving it rather than writing it down is what makes a NEW crate join the merge gate the moment
/// it joins `[workspace].members`, with no CI edit and no list to update.
pub fn ci_crates(names: &BTreeSet<String>) -> Vec<String> {
    let excluded = set(tables::roster::EXCLUDE_FROM_CI);
    names.difference(&excluded).cloned().collect()
}

/// The roster the PUBLISHED API reference documents — the CI roster, and that is a DECISION.
///
/// It is spelled as a call to [`ci_crates`] rather than as a second table, so the two answers
/// cannot drift and a new crate joins the published reference the moment it joins
/// `[workspace].members` — the property `xtask/tests/local_gate_mirrors_ci.rs`'s header
/// records four rotted hand copies for. What the equality BUYS, stated so a future divergence has
/// to argue against it: nothing reaches a public documentation page before the merge gate has
/// compiled and linted it, because the doc build cannot name a crate the gate does not.
///
/// What that leaves out is exactly [`tables::roster::EXCLUDE_FROM_CI`], and each exclusion is also correct
/// for a reader of an API reference: `vike-desktop` is the egui/wgpu GUI BINARY — a composition root
/// with no library API to call — and `vike-backfill` is a set of feature-gated collector binaries
/// whose default build documents almost nothing. A crate that genuinely belongs in the
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
