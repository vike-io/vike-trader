//! The paths that escalate a change to the full matrix or the latency gate, and the exemptions.

/// The NON-crate inputs that can move the measured binary or the measurement itself. Deliberately
/// much narrower than [`GLOBAL_PREFIXES`], and each entry earns its place:
///
///   * `Cargo.toml` (root, matched by [`super::super::selection::latency_affected`] rather than by a prefix) —
///     `[profile.release]` (opt-level 3, `lto = "thin"`) and the `[workspace.dependencies]` pins
///     vike-core's own deps inherit.
///   * `Cargo.lock` — the resolved versions of tokio/arc-swap/memmap2/serde that compile INTO the
///     fold. ⚠ The [`super::super::lockfile::lock_additive_only`] exemption is deliberately NOT honoured here:
///     "additive" proves no ALREADY-RESOLVED version moved, which is not the same claim as "the fold
///     is unchanged" — adding a brand-new dependency to vike-core is an additive lock change that
///     alters the measured binary.
///   * `rust-toolchain` — the compiler, i.e. the codegen. The gate builds `--release`.
///   * `.cargo/` — the committed Windows-scoped cargo config: RUSTFLAGS / target-cpu / the linker
///     choice all live there. It carries no SETTING today (every knob measured so far is in its
///     rejected list), but a PATH is all this trigger can see, so a comment-only edit to it fires
///     the gate too — which is stated in that file so nobody pays the bill unknowingly.
///   * `.github/workflows/ci.yml` — the gate's OWN harness: the taskset pin, SCHED_FIFO, the
///     quiesce preflight, the settles and the 3x retry. Change how it measures and it must
///     re-measure.
///   * `scripts/latency_contention.sh` — the gate's own VERDICT: it reads `/proc/pressure/cpu`
///     around each attempt and decides the job's exit code (pass, fail, or "unmeasurable"). Only
///     the latency job runs it, so an edit to it is tested by nothing else that can re-measure —
///     `crates/vike-ops/tests/ci/latency_contention_gate.rs` runs its `--selftest` and pins its call
///     sites, but cannot run it against a real p99. Edited once in its history (the PR that created
///     it), so the ~5-minute the latency box run it now buys per edit is rare.
///
/// EXCLUDED, with the reason (each was checked, not assumed):
///
///   * `rustfmt.toml` — formatting; cannot reach codegen.
///   * `deny.toml` — read by `cargo deny`, never by rustc.
///   * `justfile` — local recipes. CI derives its own plan and does not read it.
///   * `xtask/` — the planner itself; it left with its row in `GLOBAL_PREFIXES`, which says why.
///   * every OTHER `scripts/` file — none of them is run by the latency job (`ci_feature_suite.sh`,
///     `ci_lockfile_gate.sh` and the rest drive other jobs). ⚠ This line read "`scripts/*` drive
///     OTHER jobs" while the latency job was running `latency_contention.sh` unnamed above.
///   * `.github/*` (other) — the other workflows (release/deny/jforex-bridge/ctrader-proto), plus
///     the `rust-ci-setup` composite action, which the `plan` job deliberately does NOT use.
///   * `docs/`, `CLAUDE.md`, `CODEOWNERS` — prose.
pub const LATENCY_GLOBAL_PREFIXES: &[&str] = &[
    "Cargo.lock",
    "rust-toolchain",
    ".cargo/",
    ".github/workflows/ci.yml",
    "scripts/latency_contention.sh",
];

/// Paths that invalidate the narrow selection entirely and escalate to the FULL crate matrix.
///
/// ⚠ `settings/` WAS here until 2026-10-06. It stood because a committed file there was a RUNTIME
/// input to every test with no crate of its own to select by: `vike_model::paths::state_path`'s
/// `project_settings_dir` walk, started anywhere inside the checkout, answered `<repo>/settings/`.
/// It left because decision 0086 made every settings file dead, `.gitignore` ignores `/settings/`
/// whole and #2544 deleted the four tracked TOMLs: nothing under `settings/` is tracked or read, so
/// a change there selects nothing because there is nothing it could break. Put it back if a
/// tracked file under `settings/` is ever read again.
///
/// ⚠ `xtask/` WAS here until 2026-10-06 — in this list AND in [`LATENCY_GLOBAL_PREFIXES`] — and the
/// owner took it out (*"WE NEED TO GET RID OF THIS - I DONT SEE VALUES FROM IT JUST TIME WASTING"*).
/// It stood for a SELF-REFERENCE: the planner plans its OWN pull request, so a bug that narrows the
/// selection narrows its own PR too, which goes green and conceals itself. What it cost: every
/// planner PR ran the full matrix and the ~11-minute pinned-core latency measurement, over a change
/// that cannot move a single p99.
///
/// What still holds the planner honest without the row: a planner PR plans `xtask` plus its dev hop
/// `vike-ops`, which IS the planner's integration suite (`crates/vike-ops/tests/ci/ci_plan_gate.rs`,
/// driven over planted git topologies), and `release.yml` runs every suite and the whole roster on
/// every tag. The risk this accepts, stated: a planner bug that no `ci_plan_gate` test exercises and
/// that narrows a LATER pull request's plan is found at the release instead of on that PR. Put the
/// row back if a planner PR ever ships such a bug.
pub const GLOBAL_PREFIXES: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain",
    ".cargo/",
    ".github/",
    "rustfmt.toml",
    "deny.toml",
    "justfile",
    "scripts/",
];

/// `(prefix, suffix)` pairs that sit UNDER a [`GLOBAL_PREFIXES`] entry and do not earn its
/// escalation. A file matching one is treated as ordinary: it selects crates the normal way.
///
/// ⚠ **The only safe exemption is one where something ELSE already gates the file**, and that is
/// exactly why `.md` under `scripts/` qualifies. `gate_crates_for` force-adds [`super::gate_crates::DOC_GATE_CRATE`] on
/// any `.md` path anywhere ([`super::gate_crates::DOC_GATE_INPUT_SUFFIXES`]), and it does so from the RAW file list,
/// independently of this narrowing — so `crates/vike-ops/tests/docs/skills_gate.rs`,
/// `citation_gate.rs`, `publish_mirror_gate.rs` and `one_authority_gate.rs` still run. The gates
/// that actually care about these files are unaffected; only the 62-crate matrix goes away.
///
/// ⚠ These are NOT decorative files, and the exemption would be wrong if they were merely assumed
/// harmless. `scripts/skills/*.md` are the TEMPLATES rendered into `skills/*/SKILL.md`, which
/// `skills_gate` byte-compares; `scripts/mirror_readme.md` is rendered into the public mirror. They
/// are gated harder than most source — just not by compiling the workspace.
///
/// MEASURED over the 150 commits before this landed: 67 escalated to the full matrix, and 6 of them
/// were `scripts/` changes whose every file was `.md`. At 20-30 minutes each that is ~8% of all full
/// matrices, on a repository where 44% of commits trigger one.
///
/// ⚠ Deliberately a PAIR rather than a bare suffix. A bare `.md` exemption would also cover
/// `.github/**/*.md` — and a workflow's own README sits beside files that decide what CI runs, which
/// is not a claim this table is in a position to make.
///
/// ⚠ This is the ONLY pattern exemption, and it is not going to get a sibling. Everything else is
/// exempted BY NAME in [`GLOBAL_EXEMPT_FILES`]: a directory-wide pattern with a list of files it
/// must not cover is a carve-out whose failure mode is a NEW file — a script some CI job starts
/// running — inheriting the exemption without anybody deciding it should. Named, a new file stays
/// global until somebody adds it and the gate below has checked the job that runs it.
pub const GLOBAL_PREFIX_EXEMPT: &[(&str, &str)] = &[("scripts/", ".md")];

/// The files under [`GLOBAL_PREFIXES`] that do NOT escalate, BY NAME — matched by equality, so a
/// sibling, a subdirectory twin or a renamed copy is not covered. A file listed here selects crates
/// the normal way, and [`super::super::selection::lane_crates_for`] adds the crates whose tests read it to the test
/// LANE. A file NOT listed (and not markdown under `scripts/`) stays global: that is the default,
/// and it is the safe one.
///
/// ⚠ **The only safe exemption is one where something ELSE already gates the file.** For every name
/// below that "something" is the following, and each mechanism is required:
///
///   1. **The gate crate joins the lane.** Every listed file force-adds [`super::readers::EXEMPT_INPUT_GATE_CRATE`]
///      (vike-ops), the crate owning the gates that read or RUN these files —
///      `crates/vike-ops/tests/ci/local_gate_mirrors_ci.rs` (the justfile as a mirror of
///      `scripts/ci_feature_suite.sh`), `lockfile_gate_mirror.rs`, `api_docs_gate.rs`,
///      `publish_mirror_gate.rs`, `shell_backtick_gate.rs`, `latency_contention_gate.rs`,
///      `ci_slowest_tests_gate.rs`, `deploy_tool_table_gate.rs`, `container_image_gate.rs`,
///      `packaging_gate.rs`, `smoke_guard_gate.rs` and more. LANE only, never `affected`: vike-ops
///      triggers `light-consumers`, which no script edit can move.
///   2. **A reader OUTSIDE vike-ops joins the lane too** — [`super::readers::LANE_INPUT_READERS`], one row per file
///      another crate's test reads, plus [`super::gate_crates::DOCS_DATA_GATE_INPUTS`] for vike-docs. Lane only again:
///      `vike-fxcm` and `vike-dukascopy` trigger feature suites a script edit cannot move either.
///   3. **A script a WORKFLOW runs keeps the job that runs it.** `ci.yml`'s `plan` job runs
///      `scripts/ci_lockfile_gate.sh` and three selftests (`scripts/verify_branch.sh`,
///      `scripts/unit_drift.sh`, `scripts/assert_release_identity.sh`) on EVERY run — it has no
///      `if:` — which also covers the runs a pull request cannot reach: `unit_drift.sh` in
///      `deploy.yml`, and `assert_release_identity.sh` in a tag-only `release.yml` step; `scripts/ci_slowest_tests.sh`
///      runs in the `test` job, which mechanism 1 makes run; `scripts/build_api_docs.sh` and the two
///      files its build executes are [`super::readers::DOCS_JOB_INPUTS`]; `scripts/latency_contention.sh`, which
///      only the latency job runs, is a [`LATENCY_GLOBAL_PREFIXES`] entry. A script only a tag-, dispatch- or
///      `workflow_run`-triggered workflow runs never ran under the full `ci.yml` matrix either, so
///      for those the gate crate of mechanism 1 is what tests the change, and each has one.
///   4. **A deleted or renamed-away file stays GLOBAL** ([`super::super::selection::escalates`] checks the path still
///      exists). Citations of these paths live in every crate —
///      `crates/vike-data/tests/series_cadence_gate.rs` resolves the ones its table's prose names —
///      so a deletion can break a test no row selects, while an EDIT cannot move an existence check.
///      Rare (two deleted files in the 150 commits measured below, both in one PR), so it costs
///      nothing.
///
/// `crates/vike-ops/tests/ci/ci_plan_gate.rs` holds this list BOTH WAYS: every name is a tracked file,
/// every tracked file under `scripts/` and `.github/` is either named here or in that file's list
/// of files that stay global (each with its reason), and every script ANY workflow or action runs
/// — directly, through a `just` recipe, or through another script — is named here only if
/// mechanism 3 holds for the job that runs it or a named gate crate covers it. It also derives
/// mechanism 2's reader set from the tree both ways.
///
/// MEASURED (2026-10-03, the 150 first-parent commits on `main` before this landed, each job costed
/// at its median over 32 real push runs): 73 escalated, `scripts/` was the commonest trigger (34),
/// and exempting `scripts/` + `justfile` turned 22 of them narrow on their own, saving ~17,800
/// runner-seconds. The pure-script PRs are the ones that feel it — #2382, two deploy scripts, ran a
/// 773 s full matrix where a vike-ops-only run takes ~90 s. The justfile alone saves ~0: it never
/// decided a commit by itself, and is exempt because nothing in CI reads it but two tests.
///
/// ⚠ **`.github/` is named file by file too, and two of its files are NOT named.**
/// `.github/workflows/ci.yml` defines how every job runs (and `crates/vike-core/tests/common/latency_line.rs`
/// reads it), and `.github/actions/rust-ci-setup` is the setup every build job runs — an edit to
/// either must re-run what it changes. Every OTHER workflow is a separate workflow: path-triggered
/// on its own file (brand-assets, content, coverage, ctrader-proto, deny, feature-matrix,
/// fonts-vendor, fuzz, jforex-bridge, mutants) or run only on a tag, by hand, on a schedule or
/// after `release` (agent-eval, deploy, live-smokes, mcp-registry-publish, mirror-publish,
/// release, release-image, ureq-release-watch). The full `ci.yml` matrix never ran one of them, so
/// escalating on their edit bought nothing — while their READERS (mechanism 2: vike-dukascopy and
/// vike-strategy-builder read `release.yml`, vike-dukascopy reads `jforex-bridge.yml`, vike-docs
/// through [`super::gate_crates::DOCS_DATA_GATE_INPUTS`]) and vike-ops' workflow gates (mechanism 1) still run.
/// MEASURED over the same 150 commits: 4 commits were held global by such a workflow alone
/// (~5,300 runner-seconds, three of them single-workflow PRs costing over 1,000 each), and 4 more
/// needed both this and the `scripts/` names (~2,900).
///
/// Three names here were exemptions before the rest, and their measurements are why the rule above
/// reads the way it does:
///   * `scripts/build_api_docs.sh` (CI run 34875966216: one changed file emitted 62 `-p` flags and
///     all 14 suite legs; 164 s instead of 492 s once exempt) — and the one that found mechanism 3:
///     the `api-docs` job was the only thing that ever EXECUTED it, and nothing ran it once it
///     stopped escalating until [`super::readers::DOCS_JOB_INPUTS`] named it.
///   * `scripts/verify_branch.sh` and its fixture harness (2026-09-22) — licensed by the `plan`
///     job's selftest step, and a measured warning about what happens without one: the fixture
///     suite had been RED on `main` while nothing invoked it. A fixture that is never run cannot
///     even go red.
pub const GLOBAL_EXEMPT_FILES: &[&str] = &[
    ".github/CODEOWNERS",
    ".github/dependabot.yml",
    ".github/workflows/agent-eval.yml",
    ".github/workflows/brand-assets.yml",
    ".github/workflows/content.yml",
    ".github/workflows/coverage.yml",
    ".github/workflows/ctrader-proto.yml",
    ".github/workflows/deny.yml",
    ".github/workflows/deploy.yml",
    ".github/workflows/feature-matrix.yml",
    ".github/workflows/fonts-vendor.yml",
    ".github/workflows/fuzz.yml",
    ".github/workflows/jforex-bridge.yml",
    ".github/workflows/live-smokes.yml",
    ".github/workflows/mcp-registry-publish.yml",
    ".github/workflows/mirror-publish.yml",
    ".github/workflows/mutants.yml",
    ".github/workflows/release-image.yml",
    ".github/workflows/release.yml",
    ".github/workflows/ureq-release-watch.yml",
    "justfile",
    "scripts/assert_release_identity.sh",
    "scripts/batch_prs.sh",
    "scripts/batch_prs_selftest.sh",
    "scripts/build_api_docs.sh",
    "scripts/changelog_release.sh",
    "scripts/changelog_release_selftest.sh",
    "scripts/ci_lockfile_gate.sh",
    "scripts/ci_slowest_tests.sh",
    "scripts/cli_mcp_smoke.sh",
    "scripts/creds_audit.sh",
    "scripts/deploy_sandbox.sh",
    "scripts/deploy_sandbox_selftest.sh",
    "scripts/fetch_release_tools.sh",
    "scripts/fonts_vendor_drift.sh",
    "scripts/forbidden_tokens.ere",
    "scripts/gen_skills.sh",
    "scripts/graphify_verify.py",
    "scripts/hooks/pre-commit",
    "scripts/latency_contention.sh",
    "scripts/marketing_shots.sh",
    "scripts/new_venue.sh",
    "scripts/nonfree_release_assets",
    "scripts/pr_freshness.sh",
    "scripts/pr_freshness_selftest.sh",
    "scripts/pr_merge.sh",
    "scripts/pr_merge_selftest.sh",
    "scripts/prod2_lane.sh",
    "scripts/prod2_lane_selftest.sh",
    "scripts/publish_mirror.sh",
    "scripts/publish_starter_data.sh",
    "scripts/qa_judge.sh",
    "scripts/qa_judge_selftest.sh",
    "scripts/qa_shots.sh",
    "scripts/refuse_box_paths.sh",
    "scripts/refuse_live_credentials.sh",
    "scripts/release_container_image.sh",
    "scripts/release_container_image_selftest.sh",
    "scripts/release_fxcm_artifact.sh",
    "scripts/run_mutations.sh",
    "scripts/run_wf.sh",
    "scripts/studio.ps1",
    "scripts/unit_drift.sh",
    "scripts/verify_branch.sh",
    "scripts/verify_branch_selftest.sh",
    "scripts/vike_tradehub_windows.ps1",
];

/// True when `f` is NAMED in [`GLOBAL_EXEMPT_FILES`] or matches a [`GLOBAL_PREFIX_EXEMPT`] pair. A
/// PATH question only: whether the file still EXISTS is [`super::super::selection::escalates`]'s.
pub fn is_global_exempt(f: &str) -> bool {
    GLOBAL_EXEMPT_FILES.contains(&f)
        || GLOBAL_PREFIX_EXEMPT
            .iter()
            .any(|(prefix, suffix)| f.starts_with(prefix) && f.ends_with(suffix))
}
