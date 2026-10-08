//! What makes each `vike-ops` gate run: one row per gate, read by [`super::super::ops_gates::select`].
//!
//! # Why this table exists
//!
//! `vike-ops` is selected WHOLE whenever a change can reach it (`gate_crates_for` force-adds it for any
//! `.rs`, any `.md`, any crate manifest ...), and its gates are one nextest binary each, so a change that
//! cannot affect 70 of them still paid for all 77. A row says which changes DO affect a gate, and the
//! `test` job's nextest filter then names only those binaries (`Plan::ops_gates`). Nothing else about the
//! plan moves: `vike-ops` stays in `crates=` and in the roster build, and every other crate's tests are
//! selected exactly as before.
//!
//! # How to add or change a row
//!
//! 1. The row's `gate` is the `[[test]] name` of `crates/vike-ops/Cargo.toml`, character for character: no
//!    alias, no short name. A new gate has a new `[[test]]` row there AND a row here, in the same PR.
//! 2. Pick the trigger by what the gate READS, never by what it is called:
//!    * [`Trigger::Always`] when the gate walks every tracked file, runs in well under a second, or its
//!      input set cannot be written down. A wrong "always" costs seconds; a wrong skip is a silent hole.
//!    * [`Trigger::On`] otherwise. A gate runs when ANY of its `paths` is a prefix of a changed file (a
//!      directory prefix ends in `/`; an exact file is spelled in full), OR any of its `suffixes` ends a
//!      changed file (`.rs`, `.md`, `Cargo.toml`), OR any of its `tokens` occurs in a line the diff ADDS
//!      or REMOVES. A gate that walks every `.rs` (or `.md`, or manifest) for a rule that a new file
//!      ANYWHERE can break gets the suffix, not a narrow path list.
//! 3. `why` is ONE line: what the gate reads, what it guards, and its measured test-seconds.
//! 4. Never name a `crates/vike-ops/tests/<folder>/` path in a mask: the folders are a layout, and the
//!    planner already selects a gate when its OWN sources change (the `.rs` file, or the directory beside
//!    it holding its children). Use the whole prefix `crates/vike-ops/tests/` when a gate reads other
//!    gates' sources.
//!
//! # What a row never has to say, because the planner already does
//!
//! `vike-ops` runs EVERY gate, whatever the rows say, when a file under `crates/vike-ops/src/`, the crate's
//! `Cargo.toml` or build script, or `crates/vike-ops/tests/common/` changed, when the plan is the full roster,
//! when no diff base resolves (`CI_FULL=1`, a tag, an unresolvable base), when the diff text cannot be read,
//! and when a root-manifest dependency edit reaches `vike-ops`. A gate whose own source changed always runs.
//! See `xtask/src/ci/ops_gates.rs`.
//!
//! # `links`: the second input, and why it is not a row's paths
//!
//! A gate is a test BINARY: it links the workspace crates its code names, so an edit of one of them (or of a
//! crate one of them depends on) can change its verdict although no FILE the row reads changed. That edit
//! used to select every gate, whichever crate it was. Now it selects the gates whose [`GateTrigger::links`]
//! meet the edit's reverse closure (`ops_gates::select`), UNION what the rows select as before. A gate that
//! links nothing is decided by its row alone, exactly as for an edit of a crate `vike-ops` does not link.
//!
//! `links` is written down and HELD: the drift gate re-derives it from the gate's sources (tokenized, so a
//! crate named only in a comment or a string is not a link) and fails on a difference. The table is not an
//! opinion about what a gate "really" needs. Where a gate depends on a crate through DATA rather than a
//! link, the dependence is a row's job; [`LINK_KEEPS`] is the two places where that was not trusted.
//!
//! ⚠ **A gate with NO row runs on every change** (the safe default), including a test binary that
//! `[[test]]` does not list (cargo auto-discovers the root files of a tests directory). `xtask/tests/gate_triggers_gate.rs`
//! nevertheless demands a row for every `[[test]]` name, so the default is a net under a mistake rather
//! than a way to skip the table; while `PENDING_ROWS_UNTIL_STAGE4` there still lists names, those are the
//! gates whose rows have not landed yet.
//!
//! ⚠ **The seed rows below were checked against what each gate opened** (`strace`, 2026-10-06) and its
//! module doc. They are working examples of the schema; the per-gate soundness read replaces or confirms
//! them row by row.

/// What makes a gate run. A gate with NO row runs on every change (the safe default); the drift gate
/// requires a row anyway, and [`Trigger::Always`] is how a row says "always" on purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateTrigger {
    /// The `[[test]] name` of `crates/vike-ops/Cargo.toml`.
    pub gate: &'static str,
    /// The workspace crates the gate's CODE names (`vike_model::`, `use vike_data`, `extern crate xtask`),
    /// in its root file and every file it reaches through `mod`, `#[path]` and `include!`, comments and
    /// string literals excluded. A gate links exactly these, so a change that can alter one of them (or a
    /// crate depending on it, normally) can alter the gate's verdict even when no file its row names
    /// changed: see [`super::super::ops_gates::select`]. Empty = the gate links no workspace crate and its
    /// row alone decides. DERIVED, then written down: `xtask/tests/gate_triggers_gate.rs` re-derives it
    /// from the gate's sources with a real tokenizer and fails on any difference, so a gate that starts to
    /// name a crate fails there until this list says so. Sorted by crate name.
    pub links: &'static [&'static str],
    pub trigger: Trigger,
    /// ONE line: what the gate reads, what it guards, and its measured test-seconds.
    pub why: &'static str,
}

/// The inputs whose change selects a gate. See the module doc for how to choose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Always,
    On {
        /// A changed file whose path STARTS WITH one of these: a directory (`dir/`), an exact file, or a
        /// STEM naming a file and its directory together (`crates/x/src/scan` = `scan.rs` + `scan/`).
        paths: &'static [&'static str],
        /// A changed file whose path ENDS WITH one of these (`".rs"`, `".md"`, `"Cargo.toml"`).
        suffixes: &'static [&'static str],
        /// The diff's ADDED or REMOVED lines contain one of these substrings.
        tokens: &'static [&'static str],
    },
}

/// An edit that runs a gate although the gate's code does not link the crate: "not sure, so keep running".
/// It is added to the gate's [`GateTrigger::links`] by `ops_gates::select`; the drift gate holds that it
/// names a gate, names a workspace member, and is not already a derived link (a keep that repeats a link
/// says nothing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkKeep {
    /// The `[[test]] name` of the gate.
    pub gate: &'static str,
    /// The crate whose edit runs the gate (or an edit of a crate it depends on, normally).
    pub edit_of: &'static str,
    /// Why the row alone is not trusted for an edit of that crate.
    pub why: &'static str,
}

/// The keeps. Both gates read pin tables that name files of `vike-secrets`; a token row sees an added or
/// removed writer CALL, but a pinned file that was deleted or renamed is seen only through the tokens of the
/// removed lines. Together the two gates cost about two test-seconds, so a row gap is not worth betting on.
pub const LINK_KEEPS: &[LinkKeep] = &[
    LinkKeep {
        gate: "credential_writer_gate",
        edit_of: "vike-secrets",
        why: "its pin tables name vike-secrets source files (src/store/journal.rs, src/db/migrate.rs, src/store/accounts.rs ...); a deleted or renamed pinned file shows only in removed-line tokens; 1.2 s",
    },
    LinkKeep {
        gate: "profile_writer_gate",
        edit_of: "vike-secrets",
        why: "its pin tables name vike-secrets source files (src/profile_store.rs and its directory, src/settings.rs ...); a deleted or renamed pinned file shows only in removed-line tokens; 0.8 s",
    },
];

/// One row per `vike-ops` gate. A gate missing here runs on every change.
pub const GATE_TRIGGERS: &[GateTrigger] = &[
    // ---- architecture ------------------------------------------------------------------------------------------------
    GateTrigger {
        gate: "layer_gate",
        links: &[],
        trigger: Trigger::On { paths: &[], suffixes: &["Cargo.toml"], tokens: &[] },
        why: "reads only the root and every member Cargo.toml (layer rank, normal/dev/build edges, VIKE_FREE_CRATES, harness fence); guards down-only layering; 0.50 s",
    },
    GateTrigger {
        gate: "named_run_closure_gate",
        links: &[],
        trigger: Trigger::On {
            paths: &[
                "crates/vike-strategy-plugin/template/Cargo.toml.in",
                "docs/decisions/0064-a-named-run-carries-no-source.md",
                "docs/decisions/0089-the-user-code-fence-is-held-by-name-not-by-rank.md",
                "docs/decisions/0090-the-simulator-sits-alongside-the-live-core-by-name.md",
                "docs/decisions/0091-the-live-core-names-no-transport-and-no-node-client.md",
            ],
            suffixes: &["Cargo.toml"],
            tokens: &[],
        },
        why: "reads every member Cargo.toml + the plugin template Cargo.toml.in and stats the four fence decision records; guards the five dependency-closure fences; 0.66 s",
    },
    GateTrigger {
        gate: "test_surface_gate",
        links: &["vike-model"],
        trigger: Trigger::On { paths: &[], suffixes: &[".rs", "Cargo.toml"], tokens: &[] },
        why: "walks every member src/**.rs module tree (a new unreferenced file, a moved mod line or a dropped cfg breaks it) + every Cargo.toml; guards test doubles/test features out of shipped code; 15.49 s",
    },
    GateTrigger {
        gate: "paper_mount_arming_gate",
        links: &["vike-model"],
        trigger: Trigger::On {
            paths: &[
                "crates/vike-mount/src/",
                "crates/vike-paper/src/",
                "crates/vike-model/src/libm_walk.rs",
                "crates/vike-model/src/scan.rs",
                "crates/vike-model/src/scan/",
            ],
            suffixes: &[],
            tokens: &["PaperExecutionClient", "with_halt_path", "#[cfg(test)]", "#[cfg(all(test,"],
        },
        why: "scans crates/*/src/**.rs for PaperExecutionClient constructions (SITES rows live in vike-mount/vike-paper src; test-module files exempt via cfg attrs); guards that every mount arms the HALT sentinel; 2.60 s",
    },
    GateTrigger {
        gate: "clock_pin",
        links: &["vike-model", "vike-ops"],
        trigger: Trigger::On {
            paths: &["crates/vike-model/src/libm_walk.rs"],
            suffixes: &["Cargo.toml"],
            tokens: &[
                "Instant::now",
                "Local::now",
                "SystemTime::now",
                "UNIX_EPOCH.elapsed",
                "Utc::now",
                "now_unix_secs",
                "now_ms",
                "now_ns",
                "now_us",
                "#[cfg(test)]",
                "#[cfg(all(test,",
            ],
        },
        why: "scans every .rs of 11 determinism-critical crates for ambient clock reads (CLOCK_PIN) + their Cargo.toml for rand deps; guards backtest==paper==live; 3.79 s",
    },
    GateTrigger {
        gate: "parity_fold_gate",
        links: &["vike-model"],
        trigger: Trigger::On {
            paths: &[
                "crates/bridges/polymarket/src/",
                "crates/vike-analytics/src/",
                "crates/vike-backtest/src/",
                "crates/vike-exec/src/",
                "crates/vike-indicators/src/",
                "crates/vike-marketdata/src/",
                "crates/vike-model/src/",
                "crates/vike-options/src/",
                "crates/vike-orderflow/src/",
                "crates/vike-sim/src/",
                "crates/vike-strategy/src/",
            ],
            suffixes: &[],
            tokens: &[],
        },
        why: "counts .sum::<f64>() per crate in the src/ trees of the 11 NAIVE_FLOAT_FOLDS crates (cfg(test) regions/files skipped, pysum.rs must exist); ratchets naive float folds; 1.34 s",
    },
    // ---- hygiene -----------------------------------------------------------------------------------------------------
    GateTrigger {
        gate: "duplicate_shape_gate",
        links: &[],
        trigger: Trigger::On { paths: &[], suffixes: &[".rs"], tokens: &[] },
        why: "masks and shape-matches every .rs under crates/ (closing-side ternary, qty*price*mult product, workspace_root re-spelling in vike-ops tests) against ALLOWLIST counts; 10.87 s",
    },
    GateTrigger {
        gate: "compile_time_path_gate",
        links: &["vike-model"],
        trigger: Trigger::On {
            paths: &[
                "crates/vike-backfill/src/bin/ingest_bench_bars.rs",
                "crates/vike-backfill/src/cli.rs",
                "crates/vike-backtest/src/binutil.rs",
                "crates/vike-datahub/src/datahub_cli/boot.rs",
                "crates/vike-desktop/src/main.rs",
                "crates/vike-model/src/libm_walk.rs",
            ],
            suffixes: &["Cargo.toml"],
            tokens: &[
                "CARGO_MANIFEST_DIR",
                "resolve_store_root_from",
                "#[cfg(test)]",
                "#[cfg(all(test,",
            ],
        },
        why: "scans non-test member src/**.rs for the compile-time manifest-dir macro keyed (file, enclosing fn) against COMPILE_TIME_PATH_PIN + root/member/exclude Cargo.toml; 6.70 s",
    },
    GateTrigger {
        gate: "journal_scratch_gate",
        links: &[],
        trigger: Trigger::On { paths: &[], suffixes: &[".rs"], tokens: &[] },
        why: "scans every .rs under crates/ for a temp_dir path minted without a self-deleting guard (strict for vike-core, TREE_PIN elsewhere); 11.12 s",
    },
    GateTrigger {
        gate: "mojibake_gate",
        links: &[],
        trigger: Trigger::Always,
        why: "reads EVERY git-tracked text file (any extension) for double-encoded UTF-8; 0.53 s, no input set short of the whole tree",
    },
    GateTrigger {
        gate: "path_key_gate",
        links: &["vike-model"],
        trigger: Trigger::Always,
        why: "resolves every module-level const path-key literal in tracked .rs against `git ls-files`; ANY add/delete/rename of ANY tracked file (or a new top-level dir) can strand a key; 5.49 s",
    },
    GateTrigger {
        gate: "planted_exec_gate",
        links: &[],
        trigger: Trigger::On {
            paths: &[
                "crates/vike-agent-eval/tests/claude_cli.rs",
                "crates/vike-bridge-core/tests/halt_default_path.rs",
                "crates/vike-cli/tests/common/mod.rs",
                "crates/vike-cli/tests/secrets_cli/list.rs",
                "crates/vike-config/src/removed_tests.rs",
                "crates/vike-data/src/store/datafusion_hist/unwritable_root_tests.rs",
                "crates/vike-ops/tests/common/mod.rs",
                "crates/vike-secrets/tests/reads/profile_store.rs",
                "crates/vike-secrets/tests/read_only_store.rs",
                "crates/vike-strategy-builder/tests/child_env.rs",
                "crates/vike-tradehub/src/telegram/ledger.rs",
            ],
            suffixes: &[],
            tokens: &["from_mode", "0o"],
        },
        why: "scans every .rs under crates/ for exec-bit Permissions::from_mode sites against SITES counts and checks the PlantedExec rows name a cure; guards the ETXTBSY plant race; 7.36 s",
    },
    GateTrigger {
        gate: "system_temp_gate",
        links: &["vike-model"],
        trigger: Trigger::On {
            paths: &[
                "crates/vike-core/src/replay/refold.rs",
                "crates/vike-strategy-builder/src/render.rs",
                "crates/vike-core/src/run_profile/consumption_gate.rs",
                "crates/vike-core/src/runtime/tests/apply/combo.rs",
                "crates/vike-core/src/runtime/tests/deadman.rs",
                "crates/vike-core/src/runtime/tests/runtime_mount.rs",
                "crates/vike-core/src/runtime/tests/safe_state.rs",
                "crates/vike-exec/src/execution_engine/execution_engine_tests.rs",
                "crates/vike-mm/src/tests/mod.rs",
                "crates/vike-tradehub/src/tradehub_cli/tests/feed_splice/deribit.rs",
                "crates/vike-catalog/src/persist.rs",
                "crates/vike-ml/src/train/cli.rs",
                "crates/vike-model/src/libm_walk.rs",
                "crates/vike-model/src/paths/state_path.rs",
                "crates/vike-model/src/paths/state_path/",
                "crates/vike-model/src/scratch.rs",
            ],
            suffixes: &["Cargo.toml"],
            tokens: &["temp_dir", "#[cfg(test)]", "#[cfg(all(test,"],
        },
        why: "scans non-test member src/**.rs for env::temp_dir sites keyed (file, fn) against SYSTEM_TEMP_PIN, stats the cfg(test) canaries and the project_tmp_dir_from/ScratchDir homes, checks tempfile is never a normal dep; 3.96 s",
    },
    GateTrigger {
        gate: "temp_path_gate",
        links: &["vike-model"],
        trigger: Trigger::On { paths: &[], suffixes: &[".rs"], tokens: &[] },
        why: "scans every git-tracked .rs (tests/ files whole, src/ from the first cfg(test)) for a fixed-name path under the system temp root; guards cross-user /tmp collisions; 9.17 s",
    },
    GateTrigger {
        gate: "tracing_capture_gate",
        links: &["vike-model"],
        trigger: Trigger::On {
            paths: &[
                "crates/vike-log/src/capture.rs",
                "crates/bridges/binance/tests/feed_status_journal.rs",
                "crates/vike-bridge-core/tests/depth_fault_journal.rs",
                "crates/vike-tradehub/tests/control_roundtrip.rs",
                "crates/vike-tradehub/tests/deadman_absent_warning.rs",
                "crates/vike-tradehub/tests/live_tier_not_wired_log.rs",
                "crates/vike-tradehub/tests/log_capture/mod.rs",
                "crates/vike-tradehub/tests/settings_write_audit.rs",
                "crates/vike-tradehub/tests/telegram_control.rs",
            ],
            suffixes: &[],
            tokens: &["with_default", "set_default", "with_subscriber", "Subscriber"],
        },
        why: "scans every .rs under crates/ for scoped tracing defaults and `impl Subscriber` outside the vike-log capture door (SUBSCRIBER_IMPL_PIN rows), checks the door's init order; 3.96 s",
    },
    GateTrigger {
        gate: "unsafe_and_toolchain_gate",
        links: &["vike-model"],
        trigger: Trigger::On {
            paths: &[
                "rust-toolchain.toml",
                ".github/workflows/",
                ".github/actions/",
                "crates/bridges/fxcm/build.rs",
                "crates/bridges/fxcm/tests/fxcm_reconcile_parse.rs",
                "crates/bridges/fxcm/src/loader.rs",
                "crates/bridges/fxcm/src/sys.rs",
                "crates/vike-core/src/counters.rs",
                "crates/vike-desktop/src/chart_gpu.rs",
                "crates/vike-journal/src/segment.rs",
                "crates/vike-journal/src/writer.rs",
                "crates/vike-strategy-plugin/src/abi.rs",
                "crates/vike-strategy-plugin/src/fingerprint.rs",
                "crates/vike-strategy-plugin/src/guest.rs",
                "crates/vike-strategy-plugin/src/host.rs",
                "crates/vike-strategy-plugin/src/host/book.rs",
                "crates/vike-strategy-plugin/src/host/thunks.rs",
                "crates/vike-strategy-plugin/src/loader.rs",
                "crates/vike-model/src/libm_walk.rs",
            ],
            suffixes: &["Cargo.toml"],
            tokens: &["unsafe"],
        },
        why: "counts the `unsafe`/unsafe_code word per member src/ + build.rs (+ tests/ of exempt crates) against UNSAFE_SITES/EXEMPT, reads every [lints] posture, and reads the toolchain pin in rust-toolchain.toml/Cargo.toml/.github; 41.32 s",
    },
    GateTrigger {
        gate: "shipped_box_name_gate",
        links: &["vike-model"],
        trigger: Trigger::On {
            paths: &["scripts/forbidden_tokens.ere"],
            suffixes: &[".rs"],
            tokens: &[],
        },
        why: "tokenizes every .rs under any crates/**/src (string literals only, cfg(test) skipped) for forbidden box names/paths/RFC-1918 quads, pinned to scripts/forbidden_tokens.ere; 8.03 s",
    },
    GateTrigger {
        gate: "test_registry_gate",
        links: &[],
        trigger: Trigger::On {
            paths: &["crates/vike-ops/tests/", "crates/vike-ops/Cargo.toml"],
            suffixes: &[],
            tokens: &[],
        },
        why: "walks crates/vike-ops/tests/ and parses crates/vike-ops/Cargo.toml [[test]] rows; guards that every gate file is registered and every child is #[path]-reached; 0.36 s",
    },
    // ---- settings_secrets (c2) ---------------------------------------------------------------------------------------------
    GateTrigger {
        gate: "settings_registry",
        links: &["vike-model", "vike-ops"],
        trigger: Trigger::On {
            paths: &[
                // compiled-in inputs of the gate binary (a change here changes what the gate COMPUTES, with no token in any scanned line)
                "crates/vike-ops/src/settings", // all_settings(): SETTINGS rows + rows/ + the generated GENERATED_KEY_GRID
                "crates/vike-model/src/scan", // scan.rs + scan/: find_env_reads / find_map_lookups / resolve_arg / find_calls ...
                "crates/vike-model/src/libm_walk.rs", // fold_test_module_files (the walk's test-module re-inlining)
                "crates/vike-model/src/credential_keys.rs",
                "crates/vike-model/src/credential_keys_tests.rs", // lookup_keys(): the key grid the generated-key direction demands rows for
                "crates/vike-model/src/venues/mod.rs", // VENUES: the roster the grid multiplies over
                "crates/vike-model/src/venues/attribution.rs", // attribution_for: which venues contribute attribution keys
                // the credential store's own files: the gate derives them (definer files + their top-level module family) and SKIPS them
                "crates/vike-secrets/src/store",
                "crates/vike-secrets/src/dotenv.rs",
                "crates/vike-secrets/src/dotenv_tests.rs",
                "crates/vike-bridge-core/src/credentials.rs",
                // path-keyed exception tables (SRC_TEST_MODULE_OVERRIDES cuts at the file's trailing `#[cfg(test)]`; DYNAMIC_ALLOWLIST)
                "crates/bridges/bybit/src/exec.rs",
                "crates/bridges/polymarket/src/exec_plane/client.rs",
                "crates/bridges/polymarket/src/exec_plane/l1.rs",
                "crates/bridges/polymarket/tests/chain_settlement_smoke.rs",
                "crates/vike-cli/src/cmd/config.rs",
                "crates/vike-backfill/src/vike_archive.rs",
                "crates/vike-backfill/src/events_api.rs",
            ],
            suffixes: &[],
            tokens: &[
                // find_env_reads: `env::var(` / `env::var_os(` and the bare/aliased spellings a `use std::env…` line brings in
                "var(",
                "var_os(",
                "use std::env",
                // observed_process_env_sweeps: SWEEP_CALLS = ["env::vars_os()", "env::vars()"]
                "env::vars",
                // find_lookup_sites (a `.get(CONST)` whose const resolves to an env-shaped name) and const_table (crate-wide const resolution)
                ".get(",
                ": &str = \"",
                ": &'static str = \"",
                // generated_key_sites: a call of one of GENERATED_KEY_BUILDERS composes the whole key grid
                "attribution_key(",
                "credential_key(",
                "names_for_prefix(",
                // credential-store scan: CREDENTIAL_STORE_READERS (16 names) by prefix, then the hand-rolled shape (store file name / path builders)
                "load_workspace_",
                "resolve_account",
                "resolve_project",
                ".env\"",
                "SECRETS_FILE",
                "project_secrets_path",
                "workspace_dotenv_path",
                // find_map_lookups' env-shaped literals: every ENV_PREFIXES entry (quote + prefix) ...
                "\"VIKE_",
                "\"POLY_",
                "\"POLYMARKET_",
                "\"BINANCE_",
                "\"BYBIT_",
                "\"OKX_",
                "\"DERIBIT_",
                "\"ASTER_",
                "\"HYPERLIQUID_",
                "\"ALPACA_",
                "\"IG_",
                "\"OANDA_",
                "\"FXCM_",
                "\"DUKASCOPY_",
                "\"JFOREX_",
                "\"CTRADER_",
                "\"IBKR_",
                "\"IBAPI_",
                "\"DATABENTO_",
                "\"TARDIS_",
                "\"PMXT_",
                "\"EOD_",
                "\"GAMMA_",
                "\"RUST_LOG",
                "\"JAVA_HOME",
                "\"FCSDK_DIR",
                // ... and every ENV_EXACT_NAMES entry (whole literal)
                "\"HOME\"",
                "\"USERPROFILE\"",
                "\"XDG_DATA_HOME\"",
                "\"LOCALAPPDATA\"",
                "\"CARGO\"",
                // string_literals' quote-parity tracker does not understand a raw string holding a quote, nor `'\"'`: adding one flips every later literal
                "#\"",
                "'\\\"'",
            ],
        },
        why: "walks every crates/**/*.rs for env reads, env-shaped literals, const resolution, key-grid builders and credential-store readers against all_settings() + 3 pins (67.43 s); tokens/paths are the scan patterns of vike_model::scan, not a crate mask",
    },
    GateTrigger {
        gate: "settings_remedy_gate",
        links: &[],
        trigger: Trigger::On {
            paths: &[],
            suffixes: &[],
            tokens: &[
                "<project>/settings/",
                "vike-cli config compare",
                "vike-cli config adopt --undo",
            ],
        },
        why: "per-line ban over every crates/**/src non-comment line: a settings-file path after `<project>/settings/` or a deleted verb (3.01 s); exact — a line can only turn it red by carrying one of these",
    },
    GateTrigger {
        gate: "settings_row_writer_gate",
        links: &["vike-model"],
        trigger: Trigger::On {
            paths: &[
                "crates/vike-secrets/src/settings", // PRIMITIVE_MENTIONS parent + its folded test child (a dropped `mod` line re-attributes the child)
                "crates/vike-secrets/src/lib.rs",
                "crates/vike-config/src/write.rs",
                "crates/vike-model/src/scan",
                "crates/vike-model/src/libm_walk.rs",
            ],
            suffixes: &[],
            tokens: &["write_setting_row", "set_setting_within", "RowSync"],
        },
        why: "who may mention write_setting_row_in / call write_setting_row (+ 2 retired names) in crates/**/src, comment-stripped and test-folded against 2 pins (2.86 s)",
    },
    GateTrigger {
        gate: "profile_writer_gate",
        links: &["vike-model"],
        trigger: Trigger::On {
            paths: &["crates/vike-model/src/scan"],
            suffixes: &[],
            tokens: &["store_profile", "set_active", "clear_active", "OperatorWrite::claim"],
        },
        why: "who may call the 4 profile-row writers in crates/**/src (find_calls) against WRITER_CALLERS, all under vike-cli (1.73 s)",
    },
    GateTrigger {
        gate: "profile_risk_readers_gate",
        links: &["vike-model"],
        trigger: Trigger::On {
            paths: &[
                "crates/vike-secrets/src/settings", // READERS/WRITERS parent + folded test child
                "crates/vike-secrets/src/lib.rs",
                "crates/vike-model/src/scan",
                "crates/vike-model/src/libm_walk.rs",
            ],
            suffixes: &[],
            tokens: &["read_profile_risk", "write_profile_risk"],
        },
        why: "who mentions the retired profile_risk mirror's reader/writer names in crates/**/src (comment-stripped, test-folded) against READERS/WRITERS (6.43 s)",
    },
    GateTrigger {
        gate: "run_profile_row_reader_gate",
        links: &["vike-model"],
        trigger: Trigger::On {
            paths: &[
                "crates/vike-secrets/src/profile_store", // ROW_READERS parent + render.rs + folded render_tests child
                "crates/vike-model/src/scan",
                "crates/vike-model/src/libm_walk.rs",
            ],
            suffixes: &[],
            tokens: &["render_run_toml", "rows_to_run_profile"],
        },
        why: "who may turn stored rows into a run profile (render_run_toml / rows_to_run_profile mentions in crates/**/src) against ROW_READERS (5.84 s)",
    },
    GateTrigger {
        gate: "venue_fields_gate",
        links: &["vike-model"],
        trigger: Trigger::On {
            paths: &[
                "crates/vike-model/src/venues/venue_fields.rs", // VENUE_FIELDS, compiled in
                // FIELD_READERS / LOOKUP_HELPERS files, read verbatim by path (needle must still stand outside comments) ...
                "crates/vike-tradehub/src/feeds",
                "crates/bridges/binance/src/mount.rs",
                "crates/bridges/binance/src/perp_user_data.rs",
                "crates/bridges/bybit/src/mount.rs",
                "crates/bridges/bybit/src/user_data.rs",
                "crates/bridges/dukascopy/src/config.rs",
                "crates/bridges/fxcm/src/config.rs",
                "crates/bridges/vike-ibkr/src/config.rs",
                "crates/bridges/polymarket/src/egress.rs",
                "crates/bridges/polymarket/src/market_feed.rs",
                "crates/bridges/polymarket/src/exec_plane/mount.rs",
                "crates/bridges/polymarket/src/exec_plane/exec.rs",
                // ... and every other current user of `SettingTier::` (the lookup scan reads `SettingTier::X,` then the literal across newlines)
                "crates/vike-app-core/src/ui/tool_views/stored.rs",
                "crates/vike-bridge-core/src/credentials.rs",
                "crates/vike-cli/src/cmd/config/venue.rs",
                "crates/vike-datahub/src/feeds/venues.rs",
                "crates/vike-secrets/src/venue_setting.rs",
            ],
            suffixes: &[],
            tokens: &["SettingTier::"],
        },
        why: "VENUE_FIELDS vs 23 FIELD_READERS rows (each needle must still stand in its named file) + every `SettingTier::X, \"field\"` lookup in crates/**/src (1.27 s)",
    },
    GateTrigger {
        gate: "bridge_inputs_gate",
        links: &["vike-model", "vike-secrets"],
        trigger: Trigger::On {
            paths: &[
                "Cargo.toml",      // [workspace].members -> which crates/bridges/* dirs are walked
                "crates/bridges/", // every bridge src/**.rs is scanned (env access, halt path, mount sentinel, legacy names)
                "crates/vike-bridge-core/src/halt.rs",
                "crates/vike-bridge-core/src/halt_tests.rs", // every_halt_function_is_classified reads halt.rs
                "crates/vike-secrets/src/", // vike_secrets::venue_setting::declared_legacy_names (compiled in; pulls schema tiers)
                "crates/vike-model/src/venues/venue_fields.rs", // VENUE_FIELDS behind declared_legacy_names
                "crates/vike-model/src/credential_keys.rs",
                "crates/vike-model/src/credential_keys_tests.rs",
                "crates/vike-model/src/libm_walk.rs", // cfg_test_ranges / cfg_test_module_rel_files
            ],
            suffixes: &[],
            tokens: &[],
        },
        why: "bridges read no env/process-global path, every live mount hands its client the halt path, no bridge reads a declared field from the credential map: scans crates/bridges/*/src (1.85 s)",
    },
    GateTrigger {
        gate: "credential_source_roster_gate",
        links: &["vike-model"],
        trigger: Trigger::On {
            paths: &[
                "crates/vike-secrets/", // the located owner crate: src (production cut) AND its Cargo.toml (STORE_CRATE_DEPS)
                "crates/vike-bridge-core/src/credentials.rs", // ALSO_SCANNED
                "crates/vike-model/src/paths/state_path", // ALSO_SCANNED (state_path.rs + project_dirs.rs + project_root.rs)
                "crates/vike-model/src/scan",
                "crates/vike-model/src/libm_walk.rs",
            ],
            suffixes: &[],
            tokens: &["= \"node.env\"", "= \"secrets.env\"", "= \"vike.db\""], // a SECOND crate declaring the three STORE_NAMES in a const table
        },
        why: "ingress sites, file-name spellings and migration resolvers of the credential-store crate (located by its const table of node.env/secrets.env/vike.db) vs SOURCES + STORE_CRATE_DEPS (20.99 s)",
    },
    GateTrigger {
        gate: "credential_writer_gate",
        links: &["vike-model"],
        trigger: Trigger::On {
            paths: &["crates/vike-model/src/scan"],
            suffixes: &[],
            tokens: &["save_credentials", "migrate(", "set_venue_account_id", "edit_account"],
        },
        why: "who may call the 9 credential writers (save_credentials*, migrate, set_venue_account_id*, edit_account*) in crates/**/src against WRITER_CALLERS (3.79 s)",
    },
    GateTrigger {
        gate: "credential_sweep_gate",
        links: &[],
        trigger: Trigger::On {
            paths: &["scripts/creds_audit.sh", "scripts/qa_shots.sh", "scripts/marketing_shots.sh"],
            suffixes: &[],
            tokens: &[],
        },
        why: "runs scripts/creds_audit.sh, qa_shots.sh and marketing_shots.sh against planted databases (they must see vike.db and refuse a migrated root); reads nothing else in the repo (1.44 s)",
    },
    GateTrigger {
        gate: "node_key_store_gate",
        links: &["vike-model"],
        trigger: Trigger::On {
            paths: &[
                "crates/vike-desktop/src/", // GUI_SHELL_DIR: every file there is judged for the credential-shell rule
                "crates/vike-model/src/libm_walk.rs", // cfg_test_module_rel_files (which src files are test children)
                // every file that holds a node-key constructor / a resolve_node_keys call today: the gate's cfg(test) skipper and its
                // 11-line argument window make a NON-token edit in such a file (a continuation line, a `#[cfg(test)]`) able to change the verdict
                "crates/vike-app-core/src/backend/backend_registry.rs",
                "crates/vike-backtest/src/backtest_cli/serve.rs",
                "crates/vike-cli/src/boot.rs",
                "crates/vike-cli/src/lib.rs",
                "crates/vike-cli/src/cmd/datahub.rs",
                "crates/vike-cli/src/cmd/nodekeys.rs",
                "crates/vike-cli/src/cmd/nodekeys_tests.rs",
                "crates/vike-datahub-client/src/route.rs",
                "crates/vike-datahub/src/datahub_cli.rs",
                "crates/vike-node-proto/src/auth.rs",
                "crates/vike-node-proto/src/auth_tests.rs",
                "crates/vike-ops/src/settings/rows/gui.rs",
                "crates/vike-studio/src/backend/remote.rs",
                "crates/vike-tradehub-client/src/auth.rs",
                "crates/vike-tradehub/src/node.rs",
                "crates/vike-tradehub/src/tradehub_cli/tests",
            ],
            suffixes: &[],
            tokens: &[
                // CONSTRUCTORS
                "node_keys_from_vars(",
                "auth::from_vars(",
                "from_vars_named(",
                // VENUE_STORE_EXPRESSIONS (the ban list)
                "workspace_credentials(",
                "resolve_project(",
                "load_workspace_secrets",
                // probe rule: the resolve_node_keys call and PROBE_TOKENS
                "resolve_node_keys",
                "is_tradehub_node_key",
                "is_datahub_node_key",
                "is_platform_key",
                "is_node_key",
            ],
        },
        why: "node keys are built from the node store never the venue store: every node-key constructor + its argument, every resolve_node_keys probe, the GUI credential shell, in crates/**/src (14.64 s)",
    },
    GateTrigger {
        gate: "non_rust_credential_readers_gate",
        links: &[],
        trigger: Trigger::On {
            paths: &["scripts/", "deploy/", ".github/", "docs/ops/", "justfile"],
            suffixes: &[],
            tokens: &[],
        },
        why: "every non-.rs file under scripts/ deploy/ .github/ docs/ops/ + justfile naming secrets.env/node.env/vike.db/settings/db/.cpcreds/strategy-builder.key against SITES (0.78 s)",
    },
    GateTrigger {
        gate: "smoke_guard_gate",
        links: &[],
        trigger: Trigger::On {
            paths: &[
                ".github/workflows/live-smokes.yml", // the lane: guard steps, env, the `--test NAME` lines
                ".github/actions/", // local composite actions are scanned for VIKE_SETTINGS_DIR / VIKE_CREDENTIAL_SCOPE
                "scripts/refuse_live_credentials.sh", // the guard under test (run against fixtures AND the real checkout)
                "crates/bridges/", // the real-lane scan reads the lane tests + the `mod` files they declare (all bridge tests today)
            ],
            suffixes: &[],
            tokens: &[],
        },
        why: "runs scripts/refuse_live_credentials.sh over fixtures and over the real lane tests named by live-smokes.yml (a live/real-money request in any of them is red) (9.86 s)",
    },
    GateTrigger {
        gate: "smoke_store_parity_gate",
        links: &["vike-model"],
        trigger: Trigger::On {
            paths: &[
                "crates/vike-secrets/src/store/backend.rs", // STORE_FRONT_DOORS
                "crates/vike-secrets/src/store/scoped.rs",
                "crates/vike-secrets/src/venue_setting.rs", // GRAMMAR (+ folded venue_setting_tests.rs)
                "crates/vike-bridge-core/src/credentials.rs", // OLD_HOME (no `pub use` of the moved items)
                "crates/vike-secrets/src/schema/steps/any_tier.rs", // SCHEMA
                "crates/vike-bridge-core/tests/credential_classification.rs", // CLASSIFIER_TEST
                "crates/vike-model/src/libm_walk.rs",
            ],
            suffixes: &[],
            tokens: &["venue_setting_names"],
        },
        why: "the two store front doors never fold a venue setting, the grammar has one home, venue_setting_names has no fourth caller in crates/**/*.rs (1.76 s)",
    },
    // ---- venues ----
    GateTrigger {
        gate: "new_venue_gate",
        links: &["vike-model"],
        trigger: Trigger::On {
            paths: &["justfile", "scripts/new_venue.sh"],
            suffixes: &[".rs", "Cargo.toml"],
            tokens: &[],
        },
        why: "Walks every .rs and every Cargo.toml under crates/ plus the root manifest, justfile and scripts/new_venue.sh for scaffold markers, per-venue row sites and placeholder tags, so a new or edited file anywhere can break it; 45.6 test-s.",
    },
    GateTrigger {
        gate: "venue_subset_tables_gate",
        links: &["vike-model"],
        trigger: Trigger::On { paths: &[], suffixes: &[".rs"], tokens: &[] },
        why: "Derives subset memberships from named consts/fns and crates/bridges/*/src/mount.rs, and its UNREGISTERED ratchet scans every .rs under crates/ for venue-keyed consts, so any new .rs const can redden it; 6.0 test-s.",
    },
    GateTrigger {
        gate: "store_kind_gate",
        links: &["vike-data", "vike-model"],
        trigger: Trigger::On {
            paths: &["docs/", "scripts/", "deploy/", ".github/"],
            suffixes: &[".rs"],
            tokens: &[],
        },
        why: "Text-scans every crates/*/src .rs for store write-verb callers and commit-key producers (typed STORE_KINDS from vike-data) and checks that paths cited in row prose under crates/, docs/, scripts/, deploy/, .github/ still exist; 4.0 test-s.",
    },
    GateTrigger {
        gate: "fixture_consumption_gate",
        links: &[],
        trigger: Trigger::On { paths: &["fixtures/"], suffixes: &[".rs"], tokens: &[] },
        why: "Every file under fixtures/ must be named by some .rs under crates/ (corpus = all of crates/**.rs), so a fixture add/delete/rename or any .rs edit can orphan one; 8.5 test-s.",
    },
    GateTrigger {
        gate: "collector_dispatch_gate",
        links: &["vike-model"],
        trigger: Trigger::On {
            paths: &[
                "crates/bridges/",
                "crates/vike-datahub/src/backfill.rs",
                "crates/vike-datahub/src/backfill_tests.rs",
                "crates/vike-model/src/venues/mod.rs",
            ],
            suffixes: &[],
            tokens: &[],
        },
        why: "Reads crates/bridges/<venue>/src for KlineSource impls, vike-datahub's backfill.rs KLINE_SOURCES, and the compiled-in vike_model::VENUES (venues/mod.rs); 0.6 test-s.",
    },
    GateTrigger {
        gate: "kline_ingest_gate",
        links: &[],
        trigger: Trigger::On { paths: &[], suffixes: &[".rs", "Cargo.toml"], tokens: &[] },
        why: "Producer rows are DERIVED from store_kind.rs's bar row (any crate's file) and four compute-plane crates' src/ and manifests are scanned for venue bridges; hand-listed producers move, so any .rs/manifest; 0.2 test-s.",
    },
    GateTrigger {
        gate: "feed_stop_windows_gate",
        links: &["vike-model"],
        trigger: Trigger::On { paths: &[], suffixes: &[".rs"], tokens: &[] },
        why: "Scans crates/bridges and vike-bridge-core/src for feed-mounting files and the recorder.rs doc table whose cited files can be anywhere under crates/ (the table is hand-edited), so any .rs; 1.7 test-s.",
    },
    GateTrigger {
        gate: "feed_success_disclosure_gate",
        links: &["vike-model"],
        trigger: Trigger::On {
            paths: &[
                "crates/bridges/",
                "crates/vike-bridge-core/",
                "crates/vike-tradehub/src/",
                "crates/vike-model/src/scan",
            ],
            suffixes: &[],
            tokens: &[],
        },
        why: "Fixed SCAN_ROOTS (crates/bridges, vike-bridge-core/src, vike-tradehub/src) plus pump_spec_tests.rs's OWN_PUMP roster and vike_model::scan; 0.9 test-s.",
    },
    GateTrigger {
        gate: "wallet_frame_venues_gate",
        links: &[],
        trigger: Trigger::On { paths: &["crates/bridges/"], suffixes: &[], tokens: &[] },
        why: "Reads only crates/bridges/*/Cargo.toml and every .rs under crates/bridges/*/src to derive which venues receive a live wallet frame; 6.0 test-s.",
    },
    GateTrigger {
        gate: "live_mount_line_gate",
        links: &["vike-model"],
        trigger: Trigger::On {
            paths: &[
                "Cargo.toml",
                "crates/bridges/",
                "crates/vike-model/src/venues/mod.rs",
                "crates/vike-model/src/libm_walk.rs",
            ],
            suffixes: &[],
            tokens: &[],
        },
        why: "Reads the root [workspace].members, crates/bridges/*/Cargo.toml and */src .rs mount files, and the compiled-in vike_model::VENUES; 1.8 test-s.",
    },
    GateTrigger {
        gate: "polymarket_egress_declared_gate",
        links: &["vike-model"],
        trigger: Trigger::On { paths: &[], suffixes: &[".rs", "Cargo.toml"], tokens: &[] },
        why: "Derives the vike-polymarket dependency closure from EVERY workspace manifest and reads the binaries/sources of whichever crates that closure contains, so any manifest or .rs edit can move it; 0.7 test-s.",
    },
    // ---- gui ----  (the three GUI rows below are sound ONLY with the companion drift check; fallback rows follow)
    GateTrigger {
        gate: "ui_glyph_coverage",
        links: &["vike-model", "vike-ui-theme"],
        trigger: Trigger::On {
            paths: &[
                "crates/vike-app-core/src/",
                "crates/vike-chart/src/",
                "crates/vike-cockpit/src/",
                "crates/vike-connections/src/",
                "crates/vike-data-manager/src/",
                "crates/vike-desktop/src/",
                "crates/vike-panels/src/",
                "crates/vike-studio/src/",
                "crates/vike-ui-theme/",
                "assets/fonts/",
                "Cargo.lock",
                "crates/vike-model/src/libm_walk.rs",
                "crates/vike-ops/tests/",
            ],
            suffixes: &["Cargo.toml"],
            tokens: &["FontDefinitions", "set_fonts"],
        },
        why: "Prints every non-ASCII literal of the egui crates' src/ against the bundled fonts (assets/fonts, vike-ui-theme, the egui pin in Cargo.lock) and bans FontDefinitions::default()/.set_fonts( in any .rs under crates/ (the two tokens); 12.0 test-s.",
    },
    GateTrigger {
        gate: "ui_literal_ratchet",
        links: &["vike-model"],
        trigger: Trigger::On {
            paths: &[
                "crates/vike-app-core/src/",
                "crates/vike-chart/src/",
                "crates/vike-cockpit/src/",
                "crates/vike-connections/src/",
                "crates/vike-data-manager/src/",
                "crates/vike-desktop/src/",
                "crates/vike-panels/src/",
                "crates/vike-studio/src/",
                "crates/vike-ui-theme/ui-theme.toml",
                "crates/vike-model/src/libm_walk.rs",
                "crates/vike-ops/tests/",
            ],
            suffixes: &["Cargo.toml"],
            tokens: &[],
        },
        why: "Counts raw colours, literal sizes, weights, spacing and strokes in the egui crates' src/ (theme crate excluded) against per-file pins and the [[weight_use]] ids of vike-ui-theme/ui-theme.toml; scope derived from manifests; 22.2 test-s.",
    },
    GateTrigger {
        gate: "sentinel_fold_ratchet",
        links: &["vike-model", "xtask"],
        trigger: Trigger::On {
            paths: &[
                "crates/vike-chart/src/",
                "crates/vike-studio/src/",
                "xtask/src/",
                "crates/vike-model/src/libm_walk.rs",
            ],
            suffixes: &["Cargo.toml"],
            tokens: &[],
        },
        why: "Pins every sentinel-seeded fold in the egui_plot crates' src/ (scope = manifests declaring egui_plot) and asserts via xtask::ci::selection/tables::roster that a plot-crate change still selects vike-ops; 5.1 test-s.",
    },
    GateTrigger {
        gate: "studio_compute_key_reach_gate",
        links: &["vike-model"],
        trigger: Trigger::On {
            paths: &[
                "crates/vike-studio/",
                "crates/vike-desktop/",
                "crates/vike-app-core/",
                "crates/vike-ops/src/settings/",
                "crates/vike-ops/tests/",
                "crates/vike-model/src/libm_walk.rs",
                "crates/vike-model/src/scan",
            ],
            suffixes: &[],
            tokens: &["COMPUTE_KEY", "compute_key"],
        },
        why: "Text-scans all .rs under crates/ for a second spelling/resolver of Studio's compute-key variable (tokens) and the desktop crates + vike-ops settings row/registry gate for its one reader, one resolution and one NodeKeys construction; 17.1 test-s.",
    },
    GateTrigger {
        gate: "studio_tunnel_gate",
        links: &[],
        trigger: Trigger::On {
            paths: &[
                "justfile",
                "scripts/studio.ps1",
                "docs/ops/backtest-daemon.md",
                "crates/vike-strategy-builder/src/builder.rs",
                "crates/vike-strategy-builder/src/builder_tests.rs",
                "crates/vike-strategy-builder/src/client.rs",
                "crates/vike-config/src/config.rs",
                "crates/vike-config/src/config_tests.rs",
                "crates/vike-tradehub/src/server",
                "crates/vike-studio/src/backend/remote.rs",
                "crates/vike-studio/src/backend/remote_tests.rs",
                "crates/vike-studio/src/studio",
                "crates/vike-app-core/src/backend/backend_registry.rs",
                "crates/vike-app-core/src/backend/backend_registry_tests.rs",
            ],
            suffixes: &[],
            tokens: &[],
        },
        why: "Compares the justfile studio_* recipes, scripts/studio.ps1 and docs/ops/backtest-daemon.md against exactly ten Rust constants/witness files (bind ports, key env names, dial defaults); closed read-set; 0.4 test-s.",
    },
    GateTrigger {
        gate: "ci_excluded_gui_shell_ratchet",
        links: &["xtask"],
        trigger: Trigger::On {
            paths: &["crates/vike-desktop/", "xtask/src/"],
            suffixes: &[],
            tokens: &[],
        },
        why: "Ratchets code lines of every .rs in crates/vike-desktop (tests/ skipped) and asserts via xtask::ci that vike-desktop stays in EXCLUDE_FROM_CI and a main.rs-only change still selects vike-ops; 0.3 test-s.",
    },
    // ---- docs ----
    GateTrigger {
        gate: "citation_gate",
        links: &[],
        trigger: Trigger::On {
            paths: &["Cargo.lock"],
            suffixes: &[".rs", ".md", ".toml", ".sh", ".py", ".java", ".kts", ".yml", ".yaml"],
            tokens: &[],
        },
        why: "walks every file in the tree (index), reads comments/literals of every .rs under a workspace root, every authoritative .md and every .github/**/*.yml for `path`'s `SYMBOL`, path:NNN and \"Heading\" citations; a symbol rename, a deleted/renamed/added file of a cited extension or a moved heading reddens it; 26.11 s",
    },
    GateTrigger {
        gate: "unrun_command_gate",
        links: &[],
        trigger: Trigger::On {
            paths: &["Cargo.toml", "Cargo.lock"],
            suffixes: &[".rs", ".md"],
            tokens: &[],
        },
        why: "harvests command-plus-claim pairs from every authoritative .md and every .rs comment (corpus from root Cargo.toml workspace tables) and RUNS 11 `git grep`/`git ls-files` rows whose answers (crate-page count, symbol rosters) move with any .rs/.md add/remove/rename; 31.42 s",
    },
    GateTrigger {
        gate: "skills_gate",
        links: &["vike-model"],
        trigger: Trigger::On {
            paths: &[
                "skills/",
                "scripts/skills/",
                "scripts/gen_skills.sh",
                "crates/vike-cli/src/cmd/mcp.rs",
                "crates/vike-cli/src/cmd/mcp/tool_schemas.rs",
                "crates/vike-cli/src/exit.rs",
                "crates/vike-cli/src/lib.rs",
                "crates/vike-config/src/policy.rs",
                "crates/vike-model/src/scratch.rs",
                "crates/vike-model/src/scratch_tests.rs",
            ],
            suffixes: &[],
            tokens: &["skills", "Skills", "SKILLS", "procedures", "Procedures", "PROCEDURES"],
        },
        why: "re-renders skills/ with gen_skills.sh from its templates + the 5 .rs sources it awks (tools, WRITE_TOOLS, exits, COMMANDS, policy consts) and compares bytes; plus a git-ls-files scan for a written skill-count (an added line naming skills/procedures); 5.83 s",
    },
    GateTrigger {
        gate: "decision_index_gate",
        links: &[],
        trigger: Trigger::On { paths: &["docs/decisions/"], suffixes: &[], tokens: &[] },
        why: "reads only docs/decisions/ (README.md index rows vs every NNNN-*.md Status line, flat and recursive walk); 0.20 s",
    },
    GateTrigger {
        gate: "one_authority_gate",
        links: &[],
        trigger: Trigger::On { paths: &[], suffixes: &[".md"], tokens: &[] },
        why: "walks every .md in the tree (minus superpowers/research/content/skills trees it excludes itself) for an (identifier, number) fact spelled in two authoritative pages; 3.64 s",
    },
    GateTrigger {
        gate: "root_claude_md_size_gate",
        links: &[],
        trigger: Trigger::On { paths: &["CLAUDE.md"], suffixes: &[], tokens: &[] },
        why: "measures the byte size of the root CLAUDE.md against a ratchet in its own source (both directions); 0.04 s",
    },
    GateTrigger {
        gate: "superpowers_index_gate",
        links: &[],
        trigger: Trigger::On { paths: &["docs/superpowers/"], suffixes: &[], tokens: &[] },
        why: "docs/superpowers/README.md rows and counts vs the .md stems in docs/superpowers/plans and /specs; 0.09 s",
    },
    GateTrigger {
        gate: "user_strategy_surface_gate",
        links: &[],
        trigger: Trigger::On {
            paths: &[
                "Cargo.toml",
                "Cargo.lock",
                "crates/vike-user-strategies/Cargo.toml",
                "crates/vike-strategy-plugin/template/Cargo.toml.in",
            ],
            suffixes: &[],
            tokens: &[],
        },
        why: "parses three manifests (build-time tier, plugin template, root workspace `toml` pin) and compares their [dependencies]; toml-crate semantics matter; 0.07 s",
    },
    GateTrigger {
        gate: "docs_constants_gate",
        links: &[],
        trigger: Trigger::On { paths: &[], suffixes: &[".rs", ".md"], tokens: &[] },
        why: "CLAIMS table pins a documented default (7 authoritative pages) to a literal in ~10 .rs files; suffixes rather than the table's file list so a new CLAIMS row cannot outrun its trigger; 0.22 s",
    },
    GateTrigger {
        gate: "kill_switch_gate",
        links: &["vike-model"],
        trigger: Trigger::On {
            paths: &[],
            suffixes: &[".rs", ".toml", ".md", ".sh", ".service"],
            tokens: &[],
        },
        why: "ROWS name ~40 symbols in ~40 .rs/.toml/.service files and docs/ops/kill-switches.md cites arbitrary repo paths (crates/ docs/ deploy/ scripts/ fixtures/), all must resolve; suffixes cover the dynamic cite set; 0.12 s",
    },
    GateTrigger {
        gate: "mcp_instructions_gate",
        links: &[],
        trigger: Trigger::On { paths: &[], suffixes: &[".rs", "Cargo.toml"], tokens: &[] },
        why: "reads cmd/mcp/instructions.rs + vike-agent-eval/src/cases.rs, root Cargo.toml members, every member Cargo.toml ([[bin]]/name) and stats src/main.rs + lists src/bin/*.rs; .rs covers the infix src/bin; 0.06 s",
    },
    // ---- ci ----
    GateTrigger {
        gate: "ci_slowest_tests_gate",
        links: &["vike-model"],
        trigger: Trigger::On {
            paths: &[
                "scripts/ci_slowest_tests.sh",
                ".config/nextest.toml",
                ".github/workflows/ci.yml",
                ".github/actions/rust-ci-setup/action.yml",
                "Cargo.lock",
                "crates/vike-model/src/scratch.rs",
                "crates/vike-model/src/scratch_tests.rs",
            ],
            suffixes: &[],
            tokens: &[],
        },
        why: "runs scripts/ci_slowest_tests.sh on planted JUnit and reads nextest.toml [profile.ci.junit], ci.yml's `test` job steps and rust-ci-setup/action.yml for wiring; 0.61 s",
    },
    GateTrigger {
        gate: "latency_contention_gate",
        links: &["vike-model"],
        trigger: Trigger::On {
            paths: &[
                "scripts/latency_contention.sh",
                ".github/workflows/ci.yml",
                "crates/vike-model/src/test_support/",
            ],
            suffixes: &[],
            tokens: &[],
        },
        why: "extracts and EXECUTES the `Latency gate` step's shell from ci.yml against planted observations, plus latency_contention.sh --selftest and structural pins on both files; 3.59 s",
    },
    GateTrigger {
        gate: "pr_queue_gate",
        links: &[],
        trigger: Trigger::On {
            paths: &[
                ".github/workflows/queue.yml",
                "scripts/pr_freshness.sh",
                "scripts/pr_freshness_selftest.sh",
                "scripts/pr_merge.sh",
                "scripts/pr_merge_selftest.sh",
                "scripts/pr_queue.sh",
                "scripts/pr_queue_selftest.sh",
            ],
            suffixes: &[],
            tokens: &[],
        },
        why: "queue.yml held by its code lines (triggers, token, checkout, secret line, no store names) and `--selftest` of scripts/pr_queue.sh, pr_merge.sh, pr_freshness.sh (+ their *_selftest.sh); NOT in times.txt, measure it",
    },
    // ---- container_deploy ------------------------------------------------------------------------------------------------
    GateTrigger {
        gate: "container_image_gate",
        links: &[],
        why: "reads deploy/docker/*, the image staging script + its selftest (RUNS both selftests), the release workflows, crates/vike/src/main.rs TOOLS, REMOVED_ENV table, tradehub config.rs, the ops page, and runs `cargo metadata --no-deps` for the one bin name crates/vike builds; 53.1 s",
        trigger: Trigger::On {
            paths: &[
                "deploy/docker/",
                "deploy/sbin/vike-image-build",
                "scripts/release_container_image.sh",
                "scripts/release_container_image_selftest.sh",
                "crates/bridges/fxcm/scripts/",
                "crates/bridges/fxcm/FCSDK.linux.sha256",
                "crates/vike/",
                "crates/vike-config/src/removed",
                "crates/vike-tradehub/src/config.rs",
                "docs/ops/tradehub-container.md",
                ".github/workflows/release.yml",
                ".github/workflows/release-image.yml",
                // cargo metadata --no-deps: the workspace table, the two crates the toolchain smoke copy requires, cargo config, and the
                // toolchain (the test text-scans cargo's JSON field order, so a cargo bump can redden it)
                "Cargo.toml",
                "crates/vike-strategy-builder/Cargo.toml",
                "crates/vike-strategy-plugin/Cargo.toml",
                ".cargo/",
                "rust-toolchain.toml",
            ],
            suffixes: &[],
            tokens: &["[[bin]]", "autobins"],
        },
    },
    GateTrigger {
        gate: "multicall_gate",
        links: &[],
        why: "reads every .rs directly under crates/vike/src, crates/vike/Cargo.toml (`full` feature), release.yml, and tests/*/layer_gate.rs (TIERS row 68); 0.17 s",
        trigger: Trigger::On {
            paths: &["crates/vike/", ".github/workflows/release.yml", "crates/vike-ops/tests/"],
            suffixes: &[],
            tokens: &[],
        },
    },
    GateTrigger {
        gate: "graceful_stop_pin",
        links: &[],
        why: "reads exactly 9 files: both daemons' Cargo.toml + entry source, vike-ops Cargo.toml + src/stop.rs, the two shipped units, docs/ops/graceful-stop.md; 0.11 s",
        trigger: Trigger::On {
            paths: &[
                "crates/vike-datahub/Cargo.toml",
                "crates/vike-datahub/src/recorder.rs",
                "crates/vike-tradehub/Cargo.toml",
                "crates/vike-tradehub/src/tradehub_cli.rs",
                "crates/vike-ops/Cargo.toml",
                "crates/vike-ops/src/stop.rs",
                "deploy/vike-datahub.service",
                "deploy/vike-tradehub.service",
                "docs/ops/graceful-stop.md",
            ],
            suffixes: &[],
            tokens: &[],
        },
    },
    GateTrigger {
        gate: "deploy_layout_gate",
        links: &[],
        why: "reads every deploy/*.service + deploy/sbin/* (listing, bash -n), the ops runbook, recorder.example.toml, rust-toolchain.toml, 5 named .rs consts, and walks crates/**/*.rs for 2 live-gate env names + 4 ready-banner strings (token rows); 9.4 s",
        trigger: Trigger::On {
            paths: &[
                "deploy/",
                "docs/ops/recorder-deploy.md",
                "crates/vike-recorder/recorder.example.toml",
                "rust-toolchain.toml",
                "crates/vike-model/src/paths/state_path",
                "crates/vike-cli/src/cmd/config/mirror_recorder.rs",
                "crates/vike-datahub/src/datahub_cli/args.rs",
                "crates/vike-datahub/src/server.rs",
                "crates/vike-config/src/config.rs",
            ],
            suffixes: &[],
            // The whole-tree `.rs` walks: `unit_shape`'s live-gate values and the four `ready` banners must each be carried by a code
            // line, and the plant needle must be carried by none. A change makes the gate red only by adding/removing such a line.
            tokens: &[
                "VIKE_DATAHUB_LIVE",
                "VIKE_TRADEHUB_LIVE",
                "vike-strategy-builder:",
                "vike-datahub:",
                "--addr:",
                "\"ready\"",
                "deploy_layout_gate",
            ],
        },
    },
    GateTrigger {
        gate: "deploy_tool_root_gate",
        links: &["vike-model"],
        why: "reads every deploy/**/*.sh + ibkr scripts/login.js/xkbcomp-shim.c, RUNS deploy/vike-tool-root.sh under bash and compares with vike_model::paths::state_path::project_bin_dir_from; 0.8 s",
        trigger: Trigger::On {
            paths: &["deploy/", "crates/vike-model/src/paths/state_path"],
            suffixes: &[],
            tokens: &[],
        },
    },
    GateTrigger {
        gate: "deploy_tool_table_gate",
        links: &[],
        why: "compares scripts/fetch_release_tools.sh TOOL_TABLE with deploy/sbin/vike-trader-ci-deploy RUNTIME_TOOLS, release.yml ASSETS, deploy.yml verbs, one unit's KillMode; 0.05 s",
        trigger: Trigger::On {
            paths: &[
                "deploy/",
                "scripts/fetch_release_tools.sh",
                ".github/workflows/release.yml",
                ".github/workflows/deploy.yml",
            ],
            suffixes: &[],
            tokens: &[],
        },
    },
    GateTrigger {
        gate: "deploy_shadowed_store_gate",
        links: &["vike-model"],
        why: "RUNS deploy/ibkr-gateway/{write-ibc-config,start-gateway}.sh (which source deploy/vike-tool-root.sh) over a fake vike-cli, ibcstart.sh and java, RUNS deploy/ibkr-cpapi/run-login.sh's refusals, and reads/sh -n deploy/docker/entrypoint-thin.sh; ~1.5 s",
        trigger: Trigger::On { paths: &["deploy/"], suffixes: &[], tokens: &[] },
    },
    // ---- release ---------------------------------------------------------------------------------------------------------
    GateTrigger {
        gate: "api_docs_gate",
        links: &[],
        why: "reads scripts/build_api_docs.sh (+ RUNS its --selftest, which reads forbidden_tokens.ere), ci.yml, justfile, scripts/verify_branch.sh, xtask main.rs + ci/plan.rs; 0.19 s",
        trigger: Trigger::On {
            paths: &[
                "scripts/build_api_docs.sh",
                "scripts/forbidden_tokens.ere",
                "scripts/verify_branch.sh",
                ".github/workflows/ci.yml",
                "justfile",
                "xtask/src/main.rs",
                "xtask/src/ci/plan.rs",
            ],
            suffixes: &[],
            tokens: &[],
        },
    },
    GateTrigger {
        gate: "packaging_gate",
        links: &[],
        why: "reads crates/vike-cli/Cargo.toml (binstall template) + root Cargo.toml version, release.yml, rust-ci-setup action, and the mirror/starter/fxcm/box-guard scripts; RUNS refuse_box_paths.sh over planted files; 0.47 s",
        trigger: Trigger::On {
            paths: &[
                "crates/vike-cli/Cargo.toml",
                "Cargo.toml",
                ".github/workflows/release.yml",
                ".github/actions/rust-ci-setup/action.yml",
                "scripts/forbidden_tokens.ere",
                "scripts/nonfree_release_assets",
                "scripts/refuse_box_paths.sh",
                "scripts/publish_mirror.sh",
                "scripts/publish_starter_data.sh",
                "scripts/release_fxcm_artifact.sh",
                "crates/bridges/dukascopy/jforex-bridge/build.gradle.kts",
            ],
            suffixes: &[],
            tokens: &[],
        },
    },
    GateTrigger {
        gate: "publish_mirror_gate",
        links: &["vike-model"],
        why: "RUNS scripts/publish_mirror.sh --dry-run over the real tracked tree (ALLOW minus DENY, redacted, then forbidden-token scan) and plants REDACT entries in a copy; 25.4 s",
        trigger: Trigger::On {
            paths: &[
                "scripts/publish_mirror.sh",
                "scripts/forbidden_tokens.ere",
                "scripts/mirror_readme.md",
                // the two executables whose redaction is the `rewritten >= 1` floor (a mode change of either alone reddens it)
                "crates/bridges/fxcm/scripts/package-fcsdk-runtime.sh",
                "crates/bridges/fxcm/scripts/provision-fcsdk.sh",
                // ScratchDir (the test's own scratch guard) and the files the tree must contain
                "crates/vike-model/src/scratch.rs",
                "crates/vike-model/src/scratch_tests.rs",
                "crates/vike-model/src/lib.rs",
                "server.json",
                "LICENSE.md",
            ],
            suffixes: &[],
            // Content class: a shipped file survives the mirror's redaction with a forbidden token only for these three
            // shapes (the box-name and private-network tokens have byte-identical redaction rows, so a sed pass always removes
            // them before the scan; a bare data-mount root, a service-directory prefix and the private-range prefix are the
            // survivors). They are spelled with `concat!` so this shipped file does not itself contain the spelling.
            tokens: &[concat!("/mnt", "/data"), concat!("/srv", "/vike-"), concat!("192", ".168")],
        },
    },
    GateTrigger {
        gate: "release_checkout_ref_gate",
        links: &[],
        why: "reads exactly release.yml, mirror-publish.yml, release-image.yml and RUNS release.yml's preflight tag step under bash; 0.37 s",
        trigger: Trigger::On {
            paths: &[
                ".github/workflows/release.yml",
                ".github/workflows/mirror-publish.yml",
                ".github/workflows/release-image.yml",
            ],
            suffixes: &[],
            tokens: &[],
        },
    },
    GateTrigger {
        gate: "release_identity_gate",
        links: &[],
        why: "reads scripts/assert_release_identity.sh (+ RUNS its --selftest, git-heavy), release.yml, ci.yml `plan` job, justfile, local .github/actions chain, vike-buildinfo lib.rs + build.rs; 7.6 s",
        trigger: Trigger::On {
            paths: &[
                "scripts/assert_release_identity.sh",
                ".github/workflows/release.yml",
                ".github/workflows/ci.yml",
                ".github/actions/",
                "justfile",
                "crates/vike-buildinfo/",
            ],
            suffixes: &[],
            tokens: &[],
        },
    },
    GateTrigger {
        gate: "release_fxcm_artifact_gate",
        links: &["vike-model"],
        why: "reads+RUNS the whole release pipeline: every .github/workflows/*.yml and deploy/sbin + sudoers.d listing, release scripts, fxcm build.rs/loader.rs/packager, and walks EVERY crates/**/Cargo.toml for [[bin]] required-features; 9.9 s",
        trigger: Trigger::On {
            paths: &[
                ".github/workflows/",
                "deploy/sbin/",
                "deploy/sudoers.d/",
                "scripts/release_fxcm_artifact.sh",
                "scripts/fetch_release_tools.sh",
                "scripts/release_container_image.sh",
                "scripts/refuse_box_paths.sh",
                "scripts/forbidden_tokens.ere",
                "crates/bridges/fxcm/build.rs",
                "crates/bridges/fxcm/src/loader.rs",
                "crates/bridges/fxcm/scripts/",
                "crates/bridges/fxcm/FCSDK.linux.sha256",
            ],
            suffixes: &["Cargo.toml"],
            tokens: &[],
        },
    },
];
