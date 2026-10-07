//! The gate crates force-added on an input the reverse-dep closure cannot reach, and their inputs.

/// The crate owning the settings-registry gate (`crates/vike-ops/tests/settings/settings_registry.rs`). That
/// gate walks EVERY `.rs` file in the workspace, so an undeclared `env::var` added ANYWHERE is its
/// business — which makes selecting it by reverse-dep closure unsound. See
/// [`super::super::selection::gate_crates_for`].
///
/// ⚠ It also owns the STORE-KIND gate (`crates/vike-ops/tests/venues/store_kind_gate.rs`), for the same
/// unsoundness: that gate checks each declared commit-key template verbatim against the PRODUCER
/// file that builds it, and those producers live in other crates —
/// `crates/vike-journal/src/materialize.rs` reaches no vike-data dependent at all, so a renamed
/// `vike_journal::materialize` commit key would merge green and redden the next unrelated vike-data
/// PR. It moved here from vike-data's `tests/` on 2026-10-03, deleting a `STORE_KIND_GATE_CRATE`
/// rule that force-added vike-data (and so its DataFusion suites) to the lane on every `.rs`
/// change; the gate's module doc carries the measurement.
pub const SETTINGS_GATE_CRATE: &str = "vike-ops";

/// The crate owning the gates that read EVERY workspace member's `Cargo.toml` — selected by a change
/// to any crate's manifest, whichever crate owns it. Same unsoundness as [`SETTINGS_GATE_CRATE`],
/// on a third input: `crates/vike-ops/tests/arch/layer_gate.rs` reads each manifest's
/// `[package.metadata.vike] layer` and its `vike-*` edges, and
/// `crates/vike-ops/tests/ci/feature_lane_coverage.rs` reads each one's `[features]` and dependency
/// declarations. A manifest belongs to its own crate, so the reverse-dep closure selects that crate
/// and its dependents — never this one, which depends on almost nothing — and until this row a
/// manifest-only change (a new feature, a new `vike-*` edge, a layer number) skipped both gates.
///
/// It also removes the most plausible way to plan the one roster crate whose default build has no
/// test (`vike-polymarket`, measured 2026-10-03) ALONE — an edit to its own manifest — which the
/// `test` job's `--no-tests=fail` would turn into a red run over zero tests. The root `Cargo.toml`
/// is not this row's: it escalates to the full roster anyway.
pub const CRATE_MANIFEST_GATE_CRATE: &str = "vike-ops";

/// The crate owning the PROSE gates — `crates/vike-ops/tests/`'s `citation_gate.rs`,
/// `one_authority_gate.rs`, `docs_constants_gate.rs`, `unrun_command_gate.rs` and
/// `decision_index_gate.rs`. Same crate as [`SETTINGS_GATE_CRATE`] and the same unsoundness for the
/// same reason, on a different input: those five walk MARKDOWN, which belongs to no workspace
/// member, so the reverse-dep closure cannot reach them either.
pub const DOC_GATE_CRATE: &str = "vike-ops";

/// File suffixes that are an input to the prose gates.
///
/// MEASURED on the real graph (2026-08-09), before [`super::super::selection::gate_crates_for`] existed — every one of
/// these produced `any=false`, i.e. the `test` job SKIPPED and not one prose gate ran:
/// `["docs/decisions/0014-….md", "docs/decisions/README.md"]` -> 0 crates (that PR's own class);
/// `["CLAUDE.md"]` -> 0; `["README.md"]` -> 0; `["content/learn/rsi.md"]` -> 0; and
/// `["crates/vike-cli/CLAUDE.md"]` -> 1 crate, which is vike-CLI, not vike-ops — so still no gate.
///
/// That is the `settings/` blind spot ([`super::escalation::GLOBAL_PREFIXES`] says why that row stood, and why it left)
/// wearing a different costume. It is NOT the same cost, though, so it deliberately does NOT
/// escalate to the full matrix the way `settings/` did: `settings/` was a RUNTIME input every test
/// could read, while a markdown file is read only by these five gates.
/// Force-adding the ONE crate that owns them buys the coverage for one lane instead of the whole
/// roster plus every feature suite on every typo fix.
///
/// `.md` is the suffix because `citation_gate.rs`'s `scanned_docs` reads EVERY tracked `.md` outside
/// its own `DOC_SCAN_EXCLUDED`, which is why `content/`'s pages count too: `content/README.md`
/// cites `crates/vike-indicators/src/indicators/` and that citation is checked.
///
/// ⚠ The DECLARED residual: rule 1 of `citation_gate.rs` resolves a cited path against the whole
/// file INDEX, so DELETING or renaming any file under a cited root (`deploy/`, `fixtures/`,
/// `assets/`, `bench/`, `content/tools/`) can rot a citation without touching a `.rs` or a `.md`.
/// Covering that means treating every path in the repo as an input, i.e. the full matrix — the cost
/// this narrow rule exists to avoid. Stated here rather than implied away.
pub const DOC_GATE_INPUT_SUFFIXES: &[&str] = &[".md"];

/// Path prefixes that are an input to the prose gates. `docs/` is a PREFIX rather than a suffix
/// match because the tree is not all markdown (`docs/ops/*.toml` are live run profiles), and a
/// non-`.md` file there is still cited by the pages around it.
///
/// ⚠ `deploy/` is here for the SAME reason and closes a MEASURED hole. [`DOC_GATE_CRATE`] is
/// `vike-ops`, which owns every gate that reads that directory — `deploy_layout_gate.rs`,
/// `deploy_tool_root_gate.rs`, `graceful_stop_pin.rs` and `container_image_gate.rs` — and `deploy/`
/// belongs to no workspace member, so before this row a PR touching only `deploy/` closed nothing,
/// `any` was "false", the `test` job SKIPPED and not one of those four gates ran. The failure that
/// exposed it: a `deploy/docker/entrypoint.sh` added on its own would merge green with the script
/// unclassified by `deploy_tool_root_gate.rs`'s `every_deploy_script_is_classified`, and then redden
/// the next unrelated PR that happened to select `vike-ops` — which is exactly the "blames the wrong
/// change" failure this whole mechanism exists to prevent.
///
/// It also partly closes the residual [`DOC_GATE_INPUT_SUFFIXES`] declares above: `citation_gate.rs`
/// resolves cited paths against the whole file index, and `deploy/` is one of the roots it names, so
/// deleting or renaming a file here can rot a citation without touching a `.rs` or a `.md`. This
/// makes that class SELECT the gate that would catch it.
///
/// Same cost shape as `docs/`, and the same argument for paying it: this force-adds ONE crate for
/// one lane rather than escalating to the full matrix, because nothing under `deploy/` is a runtime
/// input to an unrelated test.
/// ⚠ There was a third entry, `scripts/build_api_docs.sh` (`crates/vike-ops/tests/release/api_docs_gate.rs`
/// reads it). It LEFT when [`super::escalation::GLOBAL_EXEMPT_FILES`] named every other script, because
/// [`super::readers::EXEMPT_INPUT_GATE_CRATE`] now selects this same crate for every exempted path. Two
/// rows selecting one crate for one file is a second spelling waiting to disagree with the first.
pub const DOC_GATE_INPUT_PREFIXES: &[&str] = &["docs/", "deploy/"];

/// The crate owning the MCP registry manifest's drift gate
/// (`crates/vike-cli/src/cmd/mcp/tests/manifest_and_skills.rs`'s `the_registry_manifest_lists_every_tool_this_server_serves`,
/// which `include_str!`s the repo-root `server.json` and holds it equal to `tools_spec`).
///
/// ⚠ Third instance of the same unsoundness as [`SETTINGS_GATE_CRATE`], on a THIRD input class: the
/// manifest is a file at the REPOSITORY ROOT, and this is a virtual workspace — the root owns no
/// package, so [`super::super::graph::owner_of`] matches it to no crate, and a repo-root `.json` is in no
/// [`super::escalation::GLOBAL_PREFIXES`] entry and is neither `.md` nor under [`DOC_GATE_INPUT_PREFIXES`]. So a PR
/// editing ONLY `server.json` selected ZERO crates: `any` was "false" and the `test` job SKIPPED.
///
/// That left the gate firing in one direction only: it caught `tools_spec` drifting from the
/// manifest (a `.rs` change selects vike-cli) and missed the manifest drifting from `tools_spec` —
/// a tool name reordered, a `version` hand-bumped, a `_vike.tools` row edited — which is the edit
/// made when preparing a registry listing, and the direction where the published listing lies.
///
/// Same cost shape as [`DOC_GATE_CRATE`] and the same argument for paying it: force-add the ONE
/// crate that owns the gate rather than escalating to the full matrix the way [`super::escalation::GLOBAL_PREFIXES`]
/// does, because nothing under this path is a runtime input to an unrelated test.
pub const MANIFEST_GATE_CRATE: &str = "vike-cli";

/// The files that are an input to [`MANIFEST_GATE_CRATE`]'s gate. Exact paths, not a prefix: this
/// is one tracked file, and a prefix rule over the repository root would sweep every root-level
/// file into vike-cli's lane.
pub const MANIFEST_GATE_INPUTS: &[&str] = &["server.json"];

/// The crate owning the DOCS-DATA gate (`crates/vike-docs/tests/docs_data_gate.rs`), which holds the
/// rendered release assets equal to the lists that ATTACH and MIRROR them, and two constants the
/// renderer copies equal to their authorities.
///
/// ⚠ Fourth instance of the same unsoundness as [`SETTINGS_GATE_CRATE`], and it did not exist while
/// the gate lived in `vike-ops`: that crate is force-added on every `.rs` change and on the prose
/// inputs, so the gate rode along. It moved to `vike-docs` on 2026-09-26 — a crate nothing depends
/// on and no force-add named — and three of the files it reads cannot select it through the
/// reverse-dep closure: `.github/workflows/release.yml` and `scripts/publish_mirror.sh` belong to no
/// crate, and `crates/vike-core/tests/runtime_latency.rs` is a TEST source of a crate this one does
/// not depend on. The last is the live hole: a change to the latency budget alone selects
/// `vike-core` plus [`SETTINGS_GATE_CRATE`] (any `.rs`) and never this crate, so the copy the
/// published `stats.json` carries would drift in silence. `docs/superpowers/DEFERRED-BACKLOG.md` predicted exactly this for the move.
///
/// Same cost shape as [`MANIFEST_GATE_CRATE`]: force-add the ONE crate that owns the gate, keyed on
/// the exact files it reads.
pub const DOCS_DATA_GATE_CRATE: &str = "vike-docs";

/// Every repo file [`DOCS_DATA_GATE_CRATE`]'s gate reads, as EXACT paths. Neither the workflow nor
/// the script entry escalates any more ([`super::escalation::GLOBAL_EXEMPT_FILES`] names both), so this row is now the
/// ONLY thing that runs that gate on their change.
/// `crates/vike-model/src/events.rs` is in the crate's own closure and is listed for the same
/// completeness: `crates/vike-ops/tests/ci/ci_plan_gate.rs` holds this table equal to the set of files
/// that gate reads, both directions, so it cannot rot into a partial list.
pub const DOCS_DATA_GATE_INPUTS: &[&str] = &[
    ".github/workflows/release.yml",
    "crates/vike-core/tests/runtime_latency.rs",
    "crates/vike-model/src/events.rs",
    "scripts/publish_mirror.sh",
];
