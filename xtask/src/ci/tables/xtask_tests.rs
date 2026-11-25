//! xtask's own tests, split by what they READ: the table [`super::super::roster::nextest_filter`] narrows
//! `xtask`'s term of `tests=` by, and `xtask/tests/xtask_test_split_gate.rs` holds to the test sources.
//!
//! Every `.rs` edit anywhere plans `xtask` ([`super::gate_crates::SETTINGS_GATE_CRATES`]), because some of
//! its tests read the whole tree. Most of its test time is not those: it is the planner's scenarios, which
//! plant a repository of their own (`xtask/tests/ci_plan_gate.rs`'s `Fixture`, or a `tempfile` tree) and
//! run xtask's code and tables over it, and the selftests of the scripts the planner drives. They read no
//! file of the tree, so a change outside [`OWN_LOGIC_INPUTS`] cannot change their verdict.
//! MEASURED on run 44027635639 (the full roster, 2026-11-17): of xtask's 411 tests and 1,190
//! test-seconds, the [`OWN_LOGIC_MODULES`] tests are 204 and 1,036 s, the longest 62 s; the [`TREE_TESTS`]
//! are 207 and 154 s.
//!
//! # The two sets
//!
//!   * **[`TREE_TESTS`] always run** whenever `xtask` is planned, exactly as before: every test that reads a
//!     file of the tree (a scanner, a rule over the tree, anything taking the workspace as input, the
//!     workflows, the scripts, a manifest, `cargo metadata` of the workspace, `git ls-files`). A row is a
//!     module path (every test under it) or one test's full name.
//!   * **[`OWN_LOGIC_MODULES`] run only when [`own_inputs_moved`]**: the tests of those modules that no
//!     [`TREE_TESTS`] row names. A test whose inputs cannot be pinned down belongs in [`TREE_TESTS`].
//!
//! A test is in exactly one set; a test in neither fails `xtask/tests/xtask_test_split_gate.rs`, and so does
//! an own-logic test whose body, or a same-file helper it calls, reaches the tree.

/// The crate whose tests are split. A name, because the planner's roster is names.
pub const XTASK_CRATE: &str = "xtask";

/// What an [`OWN_LOGIC_MODULES`] test can read: a changed file under one of these (a `/`-ended entry is a
/// directory prefix, any other an exact repository path) runs them all.
///
/// `xtask/` is the code under test and its tables, `scripts/` the scripts the selftests run, `.github/` the
/// workflows the planner reads, the root manifest and lock the crates xtask and its tests link. The
/// toolchain pin and `.cargo/` are the compiler and the `cargo` the planted repositories run under. The
/// strategy source-hash walk is compiled into the planner by `#[path]` (`xtask/src/ci/mod.rs`), and the SDK
/// rename table's format into `xtask/src/sdk/mod.rs` the same way, so both are code under test too.
pub const OWN_LOGIC_INPUTS: &[&str] = &[
    "xtask/",
    "crates/vike-rust-strategy/build/source_hash.rs",
    "crates/vike-rust-strategy/src/sdk/rename_table.rs",
    ".github/",
    "scripts/",
    ".cargo/",
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
];

/// True when a changed file is one of [`OWN_LOGIC_INPUTS`].
pub fn own_inputs_moved(files: &[String]) -> bool {
    files.iter().any(|f| {
        OWN_LOGIC_INPUTS.iter().any(|i| if i.ends_with('/') { f.starts_with(i) } else { f == i })
    })
}

/// One module of own-logic tests: its nextest binary id, its module path, and what its tests read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OwnModule {
    /// The nextest binary id: `xtask::ci_gates` (the integration tests) or `xtask` (the library's unit tests).
    pub binary: &'static str,
    /// The module path inside that binary; its tests are `<module>::<fn>`.
    pub module: &'static str,
    /// What its tests read, besides xtask's code and tables.
    pub reads: &'static str,
}

/// A test, or every test under a module, that reads the tree: it runs whenever `xtask` is planned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeTests {
    /// The nextest binary id, as for [`OwnModule::binary`].
    pub binary: &'static str,
    /// A module path (every test under it) or one test's full name.
    pub path: &'static str,
    /// What of the tree it reads.
    pub reads: &'static str,
}

const GATES: &str = "xtask::ci_gates";
const LIB: &str = "xtask";
/// The planner scenarios' one input besides xtask itself.
const PLANTED: &str = "planted repositories only (the `Fixture` of xtask/tests/ci_plan_gate.rs, which copies in \
                       scripts/prod2_lane.sh, an input)";

/// The modules whose tests run only when [`own_inputs_moved`], minus the [`TREE_TESTS`] rows inside them.
pub const OWN_LOGIC_MODULES: &[OwnModule] = &[
    OwnModule { binary: GATES, module: "ci_plan_gate::cfg_test_module", reads: PLANTED },
    OwnModule { binary: GATES, module: "ci_plan_gate::closure_test_only", reads: PLANTED },
    OwnModule { binary: GATES, module: "ci_plan_gate::comment_only_edits", reads: PLANTED },
    OwnModule { binary: GATES, module: "ci_plan_gate::comment_only_sources", reads: PLANTED },
    OwnModule { binary: GATES, module: "ci_plan_gate::feature_test_module", reads: PLANTED },
    OwnModule { binary: GATES, module: "ci_plan_gate::graph_memo", reads: PLANTED },
    OwnModule {
        binary: GATES,
        module: "ci_plan_gate::halves",
        reads: "planted repositories (with the tree's scripts/prod2_lane.sh), the workflows",
    },
    OwnModule {
        binary: GATES,
        module: "ci_plan_gate::incremental_after_red",
        reads: "planted repositories; `the_red_run_scripts_own_selftest_passes` runs \
                scripts/ci_red_run_selftest.sh, which plants its own",
    },
    OwnModule { binary: GATES, module: "ci_plan_gate::lane_directness", reads: PLANTED },
    OwnModule { binary: GATES, module: "ci_plan_gate::latency_comment_edits", reads: PLANTED },
    OwnModule {
        binary: GATES,
        module: "ci_plan_gate::latency_reuse",
        reads: "planted repositories; the two `*_own_selftest_passes` run scripts/latency_reuse_selftest.sh \
                and `scripts/latency_bytes.sh --selftest`, which plant their own, and read those scripts",
    },
    OwnModule { binary: GATES, module: "ci_plan_gate::latency_test_edits", reads: PLANTED },
    OwnModule { binary: GATES, module: "ci_plan_gate::lock_closure_edits", reads: PLANTED },
    OwnModule { binary: GATES, module: "ci_plan_gate::ops_gate_links", reads: PLANTED },
    OwnModule { binary: GATES, module: "ci_plan_gate::ops_gate_triggers", reads: PLANTED },
    OwnModule { binary: GATES, module: "ci_plan_gate::planted_wiring", reads: PLANTED },
    OwnModule {
        binary: GATES,
        module: "ci_plan_gate::post_merge_dependency_guard",
        reads: PLANTED,
    },
    OwnModule { binary: GATES, module: "ci_plan_gate::post_merge_suites", reads: PLANTED },
    OwnModule {
        binary: GATES,
        module: "ci_plan_gate::sdk_release",
        reads: "trees and git repositories it plants under a `tempfile` directory of its own",
    },
    OwnModule { binary: GATES, module: "ci_plan_gate::selection", reads: PLANTED },
    OwnModule { binary: GATES, module: "ci_plan_gate::spawned_binary", reads: PLANTED },
    OwnModule {
        binary: GATES,
        module: "ci_plan_gate::suite_cost_packing",
        reads: "in-memory tables",
    },
    OwnModule { binary: GATES, module: "ci_plan_gate::suite_packing", reads: PLANTED },
    OwnModule { binary: GATES, module: "ci_plan_gate::test_target_source", reads: PLANTED },
    OwnModule { binary: GATES, module: "ci_plan_gate::workspace_dependency_edits", reads: PLANTED },
    OwnModule { binary: GATES, module: "ci_plan_gate::xtask_narrowing", reads: PLANTED },
    OwnModule { binary: LIB, module: "ci::docs_roster_tests", reads: "in-memory plans" },
    OwnModule {
        binary: LIB,
        module: "ci::red_run_tests",
        reads: "in-memory records and planted files",
    },
];

/// Every test that reads the tree. The module rows are whole modules; the full-name rows are the tests
/// inside an [`OWN_LOGIC_MODULES`] module that read the tree.
pub const TREE_TESTS: &[TreeTests] = &[
    // ---- whole modules ------------------------------------------------------------------------------
    TreeTests {
        binary: GATES,
        path: "build_config_gate",
        reads: "every cargo config of the tree (a walk), the root manifest",
    },
    TreeTests {
        binary: GATES,
        path: "ci_secret_mask_gate",
        reads: "every workflow and action, the justfile",
    },
    TreeTests {
        binary: GATES,
        path: "feature_lane_coverage",
        reads: "every member's manifest, the workflows and the suite script",
    },
    TreeTests {
        binary: GATES,
        path: "feature_suite_roster_gate",
        reads: "the suite script, the roster",
    },
    TreeTests {
        binary: GATES,
        path: "gate_triggers_gate",
        reads: "crates/vike-ops (its manifest and every gate source), every crate source",
    },
    TreeTests {
        binary: GATES,
        path: "just_invocation_gate",
        reads: "the justfile, the scripts it runs",
    },
    TreeTests {
        binary: GATES,
        path: "local_gate_mirrors_ci",
        reads: "the justfile, ci.yml, release.yml, the suite and lockfile scripts",
    },
    TreeTests {
        binary: GATES,
        path: "lockfile_gate_mirror",
        reads: "ci.yml, the justfile, the root manifest and lock",
    },
    TreeTests {
        binary: GATES,
        path: "shell_backtick_gate",
        reads: "every shell script and workflow",
    },
    TreeTests {
        binary: GATES,
        path: "supply_chain_gate",
        reads: "every workflow and action, the justfile, the workspace graph",
    },
    TreeTests {
        binary: GATES,
        path: "xtask_test_split_gate",
        reads: "xtask's test sources, against this table",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::buildinfo_freeze",
        reads: "ci.yml, crates/vike-buildinfo/build.rs, the justfile, scripts/prod2_lane.sh",
    },
    TreeTests { binary: GATES, path: "ci_plan_gate::draft_skip", reads: "ci.yml" },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::feature_test_users",
        reads: "every crate source naming a test-support module (the workspace graph), the workflows",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::lane_readers",
        reads: "`git ls-files` of the tree, crate manifests, crates/vike-docs/tests/docs_data_gate.rs",
    },
    TreeTests { binary: GATES, path: "ci_plan_gate::lane_triggers", reads: "the suite script" },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::multicall_trigger",
        reads: "`git ls-files` of the tree, crates/vike-gatewayhub/Cargo.toml, crates/bridges/",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::roster_features",
        reads: "the workspace graph, crates/vike-backend/Cargo.toml, crates/bridges/, the hubs' test rosters",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::strategy_lab_surface",
        reads: "the strategy templates and every crate the strategy source hash walks from them",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::test_job_filter",
        reads: "ci.yml, the workspace graph",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::workflow_blocks",
        reads: "ci.yml, the suite script, deny.toml",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::workflow_scripts",
        reads: "`git ls-files` of the tree, every workflow, the justfile",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::yaml_comment_rule",
        reads: "scripts/yaml_comment_only.sh and its samples, the workflows",
    },
    TreeTests {
        binary: LIB,
        path: "docs_bins",
        reads: "nothing, but its tests sit in an inline module this split does not name; 3 tests, 0.1 s",
    },
    TreeTests {
        binary: LIB,
        path: "ci::tables::halves",
        reads: "nothing, but its tests sit in an inline module this split does not name; 3 tests",
    },
    // ---- tests inside an own-logic module that read the tree -----------------------------------------
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::closure_test_only::no_line_of_the_trading_job_builds_a_test_target",
        reads: "ci.yml",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::comment_only_sources::a_comment_only_rust_edit_still_selects_the_citation_gate",
        reads: "crates/vike-ops's manifest and gates",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::comment_only_sources::the_sources_read_as_text_are_exactly_what_the_tree_reads",
        reads: "every crate source (the workspace graph, a walk)",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::feature_test_module::the_real_libm_walk_plans_its_dev_enablers_and_not_the_reverse_dependency_world",
        reads: "the workspace graph, crates/vike-model",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::halves::bash_and_the_planner_put_every_member_in_the_same_group_and_half",
        reads: "the workspace graph, scripts/prod2_lane.sh (it runs its crate_group)",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::incremental_after_red::ci_yml_wires_the_red_run_the_way_the_judge_reads_it",
        reads: "ci.yml, scripts/ci_red_run.sh",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::lane_directness::a_planner_change_still_reaches_the_planners_own_tests",
        reads: "crates/vike-ops/Cargo.toml",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::lane_directness::every_feature_job_crate_is_a_workspace_member",
        reads: "the workspace graph",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::lane_directness::the_backtesthub_store_guard_is_in_both_workflows_with_its_control",
        reads: "ci.yml, release.yml",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::lane_directness::the_data_job_names_exactly_its_crates",
        reads: "ci.yml",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::lane_directness::the_data_job_runs_what_the_roster_cannot_see_and_no_hub_line",
        reads: "ci.yml",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::lane_directness::the_datahub_guards_are_in_both_workflows_and_name_every_bridge",
        reads: "ci.yml, release.yml, crates/bridges/",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::lane_directness::the_datahub_job_triggers_are_what_its_features_turn_on",
        reads: "ci.yml, crates/vike-backend/Cargo.toml and the manifests its features name",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::lane_directness::the_gatewayhub_guards_are_in_both_workflows_and_name_every_bridge",
        reads: "ci.yml, release.yml, crates/bridges/",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::lane_directness::the_justfile_recipes_run_the_feature_jobs_build_lines",
        reads: "ci.yml, the justfile",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::lane_directness::the_release_lanes_skip_through_one_clause_on_the_three_root_jobs",
        reads: "release.yml",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::lane_directness::the_release_runs_both_feature_jobs_and_publish_waits_for_them",
        reads: "ci.yml, release.yml",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::lane_directness::the_release_runs_every_suite_and_publish_waits_for_them",
        reads: "release.yml",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::lane_directness::the_release_validates_the_whole_roster_under_nextest_and_still_runs_the_doctests",
        reads: "release.yml",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::lane_directness::the_trade_guards_are_in_both_workflows_and_name_every_bridge",
        reads: "ci.yml, release.yml, crates/bridges/",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::lane_directness::the_trading_job_reads_only_what_its_door_can_see",
        reads: "ci.yml, the workspace graph",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::lane_directness::the_trading_job_runs_what_the_roster_cannot_see_and_no_hub_line",
        reads: "ci.yml",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::lane_directness::the_windows_cross_triggers_name_every_crate_its_arm_checks",
        reads: "the suite script",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::latency_comment_edits::replay_the_comment_only_drop_over_listed_commit_pairs",
        reads: "the repository's own history, planned",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::latency_comment_edits::the_lexer_reads_every_source_of_the_measured_crates",
        reads: "the measured crates' sources (the workspace graph, a walk)",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::latency_reuse::the_bytes_script_never_fails_a_job",
        reads: "scripts/latency_bytes.sh",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::latency_reuse::the_bytes_script_reads_the_verdict_file_the_step_writes",
        reads: "ci.yml, scripts/latency_bytes.sh",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::latency_reuse::the_key_holds_the_job_block_and_the_profile_tables_the_planner_reads",
        reads: "ci.yml, the root manifest, scripts/latency_bytes.sh",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::latency_reuse::the_reuse_wiring_gate_can_fail",
        reads: "ci.yml",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::latency_reuse::the_reuse_wiring_holds",
        reads: "ci.yml",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::latency_reuse::the_script_never_fails_a_job",
        reads: "scripts/latency_reuse.sh",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::latency_reuse::the_script_reads_the_measuring_steps_real_name",
        reads: "scripts/latency_reuse.sh, ci.yml",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::latency_test_edits::the_harness_table_is_exactly_the_files_runtime_latency_compiles",
        reads: "crates/vike-core (the latency harness and the files it compiles in)",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::lock_closure_edits::the_latency_job_block_runs_from_its_key_to_the_next_job",
        reads: "ci.yml",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::lock_closure_edits::the_real_tree_measures_vike_cores_normal_closure_and_not_vike_data",
        reads: "the workspace graph",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::post_merge_dependency_guard::ci_yml_features_job_reads_the_plan_and_nothing_about_dependabot",
        reads: "ci.yml",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::post_merge_dependency_guard::the_exception_reads_the_same_dependency_consumers_as_the_direct_suites",
        reads: "xtask/src/ci/plan.rs and workspace_deps.rs as text",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::sdk_release::the_real_rhai_registrations_and_indicator_rows_are_read",
        reads: "the strategy template's crates and crates/vike-rhai (their sources, a walk)",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::sdk_release::the_tag_script_makes_and_pushes_the_tag_a_lane_dry_run_printed",
        reads: "scripts/sdk_cut_tag.sh (it runs it over a repository it plants)",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::spawned_binary::every_binary_driver_companion_is_a_real_crate_the_roster_builds",
        reads: "the root manifest's members",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::suite_packing::the_lightgbm_suites_reach_vike_paths_only_through_the_scratch_guard",
        reads: "scripts/ci_lightgbm_suite.sh, crates/vike-ml-training/tests, crates/vike-paths/src/scratch.rs",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::suite_packing::the_store_kind_gate_rides_vike_ops_and_vike_data_runs_only_when_affected",
        reads: "the tree through `workspace_root()`",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::test_target_source::no_compiled_source_includes_a_crate_markdown_file",
        reads: "every crate source (a walk)",
    },
    TreeTests {
        binary: GATES,
        path: "ci_plan_gate::test_target_source::no_library_source_includes_a_rust_file_out_of_a_test_directory",
        reads: "every crate source (a walk)",
    },
];

/// The nextest filterset of the tests [`own_inputs_moved`] gates: every test of an [`OWN_LOGIC_MODULES`]
/// module but the [`TREE_TESTS`] rows inside it, per binary. Names are `[a-z0-9_:]` (the gate holds that), so
/// they go into the regex unescaped.
pub fn own_logic_filter() -> String {
    let binaries: std::collections::BTreeSet<&str> =
        OWN_LOGIC_MODULES.iter().map(|m| m.binary).collect();
    let terms: Vec<String> = binaries
        .iter()
        .map(|b| {
            let modules: Vec<&str> =
                OWN_LOGIC_MODULES.iter().filter(|m| m.binary == *b).map(|m| m.module).collect();
            let kept: Vec<&str> = TREE_TESTS
                .iter()
                .filter(|t| {
                    t.binary == *b && modules.iter().any(|m| t.path.starts_with(&format!("{m}::")))
                })
                .map(|t| t.path)
                .collect();
            let mut term = format!("binary_id(={b}) & test(/^(?:{})::/)", modules.join("|"));
            if !kept.is_empty() {
                term.push_str(&format!(" & !test(/^(?:{})$/)", kept.join("|")));
            }
            format!("({term})")
        })
        .collect();
    terms.join(" | ")
}
