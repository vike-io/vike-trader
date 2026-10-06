//! The `SETTINGS` rows read by the research, backtest, strategy, data and model crates.

use super::{external_map, vike_map};
use crate::settings::{Layer, Naming, Scope, Setting};

pub(crate) const ROWS: &[Setting] = &[
    Setting {
        // The `api` model driver's key, read in BOTH of this crate's binaries — the evaluation
        // harness's `main.rs` and the unattended runner's `src/bin/vike-agent-run.rs`, each with its
        // own copy of the constant below (an IMPORTED const scans as a dynamic read, so the
        // duplication is what keeps this row visible to the gate). One row covers both: a row is
        // keyed on (name, crate).
        // Unset is not a quiet no-op: either binary REFUSES to start when a real model was asked
        // for, because a run that measured nothing and exited 0 is the failure the live-smoke lane's
        // skip-honesty step exists to prevent. The value is a secret, so nothing formats it — the
        // refusal names the VARIABLE and never the value.
        name: "ANTHROPIC_API_KEY",
        krate: "vike-agent-eval",
        scope: Scope::External,
        layer: Layer::Binary,
        naming: Naming::Konst("ANTHROPIC_API_KEY_ENV"),
        default: "<none> → `vike-agent-eval run` (unless --scripted) and `vike-agent-run run \
                  --driver api` both refuse to start",
    },
    // Cargo's own path to itself, so a re-entrant `cargo build` (rendering the cdylib
    // template's scratch crate) invokes the SAME cargo the outer build is running under,
    // never a bare `cargo` off `PATH` that might be a different toolchain entirely.
    // ⚠ This row USED to say `krate` was "vike-strategy-builder" at `Layer::BuildScript`
    // because the file reading it was named `src/build.rs` and `layer_for`'s path-suffix
    // check cannot tell a library module from a real build script — a filename coincidence
    // that let a genuine `Layer::Library` read escape `LIBRARY_PIN`'s ratchet outright. Fixed
    // by renaming that module to `src/render.rs` (see its own doc) and moving this read out to
    // `src/bin/vike-strategy-builder.rs`'s `main`, a `vars.get("CARGO")` map lookup there —
    // `Layer::Binary` at the time.
    // ⚠ **Moved AGAIN, to `Layer::Injected`, when the multicall join gave this service a
    // SECOND composition root** (`vike-backend strategy-builder`,
    // `crates/vike/src/main.rs`'s `strategy_builder_main`). The read now lives in
    // `builder::run` (`builder.rs`'s own doc argues why: one shared sequence rather than a
    // second copy of every refusal in the second composition root), which is a pure map
    // lookup over a caller-supplied `&HashMap`, not a binary reading its own
    // `std::env::vars()` — so this row moved with it, the same way `VIKE_STRATEGY_BUILDER_KEY`
    // already was.
    external_map("CARGO", "vike-strategy-builder", "cargo"),
    Setting {
        // The same read, independently, at two sites in this crate's own test tree:
        // `crates/vike-strategy-plugin/tests/load_refusals.rs`, building the fixture `.so`s that
        // file dlopens, and `crates/vike-strategy-plugin/tests/equivalence.rs`, which hands the
        // same cargo to the real builder so its plugin half is built by the toolchain running the
        // test. A row is keyed on `(name, krate)`, so both reads live under this one.
        name: "CARGO",
        krate: "vike-strategy-plugin",
        scope: Scope::External,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "cargo",
    },
    Setting {
        name: "CARGO_MANIFEST_DIR",
        krate: "vike-user-strategies",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        default: "",
    },
    // The compiled-STUDY host's build script, the twin of the row above: same fixed hop to the
    // workspace root, same fixed hop to its own committed fixture tree.
    Setting {
        name: "CARGO_MANIFEST_DIR",
        krate: "vike-user-research",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        // The agent-eval harness's SECOND model credential, read in its `main.rs` beside the API
        // key and read nowhere else. It is the long-lived subscription token `claude setup-token`
        // mints, and the Claude Code CLI reads it from the variable of the same name — so this
        // harness's whole job with it is to move it from its own environment into the one child
        // that needs it, while `SCRUB_FROM_CHILDREN` removes it from the two children that must not
        // have it (the MCP server and the paper node). Unset is not a quiet no-op: `run --driver
        // claude-cli` REFUSES to start, in the same words as the API key's refusal, because a lane
        // that measured nothing and exited 0 is the failure this harness exists to prevent. The
        // value is a secret, so nothing formats it — the refusal names the VARIABLE.
        name: "CLAUDE_CODE_OAUTH_TOKEN",
        krate: "vike-agent-eval",
        scope: Scope::External,
        layer: Layer::Binary,
        naming: Naming::Konst("CLAUDE_CODE_OAUTH_TOKEN_ENV"),
        default: "<none> → `vike-agent-eval run --driver claude-cli` and `vike-agent-run run \
                  --driver claude-cli` both refuse to start",
    },
    Setting {
        // STEP 2 flipped this Library -> Binary: `databento::client::api_key_from_env()` read the
        // workspace `.env` (and then process env) inside the ADAPTER; the `databento_backfill` bin
        // now does that read and passes the key in as a `&str`, which is what the sibling
        // `tardis`/`vike-archive` adapters in this same crate already did. The `.env` half is a
        // map lookup and the process-env fallback a direct read, so the DIRECT read wins the
        // `Naming` tie-break (the registry's documented rule) and it is `Literal` in the bin.
        name: "DATABENTO_API_KEY",
        krate: "vike-backfill",
        scope: Scope::External,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "EOD_SMOKE",
        krate: "vike-backfill",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "OUT_DIR",
        krate: "vike-user-strategies",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        default: "",
    },
    // The compiled-STUDY host's twin: its build script WRITES two generated registries here and its
    // lib/integration tests `include!` them back out.
    Setting {
        name: "OUT_DIR",
        krate: "vike-user-research",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "PMXT_SMOKE",
        krate: "vike-backfill",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        // `vike-strategy-plugin`'s own `build.rs` (`compute_fingerprint`) reads its OWN
        // TARGET/PROFILE/OPT_LEVEL/RUSTC quartet — a SEPARATE crate's build script from
        // `vike-buildinfo`'s otherwise-identical trio (its `TARGET`/`PROFILE`/`RUSTC` rows, in
        // `crates/vike-ops/src/settings/rows/platform.rs`), so a separate set of rows:
        // a row is per CRATE, not per name, and `vike-buildinfo`'s rows do not declare THIS
        // crate's read (`every_read_variable_is_declared`'s Tier-1 proof is what caught the gap —
        // the loose Tier-2 sweep alone let three of these four hide behind the sibling crate's
        // rows for over a hundred lines). Folded together into `env!("VIKE_PLUGIN_FINGERPRINT")`
        // (see that row below) via `cargo:rustc-env=`.
        name: "OPT_LEVEL",
        krate: "vike-strategy-plugin",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "PROFILE",
        krate: "vike-strategy-plugin",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "RUSTC",
        krate: "vike-strategy-plugin",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        default: "rustc",
    },
    Setting {
        name: "TARGET",
        krate: "vike-strategy-plugin",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "TARDIS_API_KEY",
        krate: "vike-backfill",
        scope: Scope::External,
        layer: Layer::Binary,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        // The cohort API root (`data.vike.io`), read by the `vikedata_backfill` bin
        // (`crates/vike-backfill/src/bin/vikedata_backfill.rs`'s `API_BASE_ENV`), which pulls it
        // out of its one `std::env::vars()` sweep and threads it into
        // `crates/vike-backfill/src/vikedata/client.rs` as a plain `&str`. `--api-base` beats it at
        // the call site.
        //
        // ⚠ Rows are keyed on `(name, krate)`, and this name had TWO of them until the research
        // crate dissolved: the study's own binary read the same variable with its own default and
        // its own flag precedence, and neither row could have declared the other's read. That is
        // the keying argument, and it survives the sibling's deletion — a SECOND crate reading
        // `VIKE_API_BASE` tomorrow needs a row of its own, not a mention in this one.
        //
        // ⚠ `MapLookup`, not `Konst`, and the distinction is the one the registry exists to make.
        // The binary takes ONE `std::env::vars()` sweep at the top of `main` and asks the resulting
        // map — `env.get(API_BASE_ENV)` — so there is no direct process-env read to spell.
        //
        // A BLANK value is IGNORED rather than honoured: `VIKE_API_BASE=` in a
        // systemd unit is an unset variable spelled clumsily, and taking it as a base URL fails
        // every fetch with a message about the empty string instead of about the missing config.
        name: "VIKE_API_BASE",
        krate: "vike-backfill",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::MapLookup,
        default: "https://data.vike.io/v1",
    },
    Setting {
        // The cohort API credential, taken from the CREDENTIAL STORE's map (never process env) by
        // the `vikedata_backfill` bin (`crates/vike-backfill/src/bin/vikedata_backfill.rs`'s
        // `API_KEY_ENV`), which passes it into `crates/vike-backfill/src/vikedata/client.rs` as a
        // parameter; that library reads no environment and opens no store. Same
        // `creds.get(NAME)`-in-a-bin idiom the `VIKE_ARCHIVE_API_KEY` row uses, and the reason this
        // row is `Layer::Binary` / `Naming::MapLookup` rather than `Injected`.
        //
        // ⚠ It needs a row DESPITE being a credential. THE GENERATED KEY GRID (the other table, in
        // `crates/vike-ops/src/settings/grid.rs`) enumerates the
        // `{VENUE}_{TIER}_API_*` family that `vike_model::credential_keys` folds out of
        // `vike_model::VENUES`; `VIKE_API_KEY` names no venue and no tier, so the grid cannot
        // produce it, `every_read_variable_is_declared` sees the literal in the bin, and without
        // this row the gate is red.
        //
        // ⚠ This name also carried TWO rows until the research crate dissolved — the study's own
        // binary read the same credential and refused before its first HTTP call. Rows are keyed on
        // `(name, krate)`, so neither row ever declared the other's read; the deletion of the
        // sibling narrows the keying argument to one row, it does not retire it.
        //
        // ABSENT is a REFUSAL, not a fallback, and for a sharper reason than in the study: an
        // unauthenticated fetch 403s part-way through a window, and this binary WRITES — a partial
        // batch lands under a commit key claiming the whole window, after which the honest fetch of
        // that window is a silent no-op forever.
        name: "VIKE_API_KEY",
        krate: "vike-backfill",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        // data.vike.io archive backfill (vike-backfill's `vike-archive` feature): the
        // `vike_archive_backfill` bin reads this via `dotenv.get("VIKE_ARCHIVE_API_KEY")` (the
        // MapLookup idiom `TARDIS_API_KEY` also uses) and passes it into `vike_archive::
        // ArchiveClient::new` as a plain parameter — the library itself never reads env/`.env`.
        name: "VIKE_ARCHIVE_API_KEY",
        krate: "vike-backfill",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        // events_api.rs's #[ignore]d live smokes (self-skip without a real token id) read this
        // purely to name a real token_id for the network round trip — the same TestOnly idiom as
        // `crates/vike-ops/src/settings/rows/bridges.rs`'s `ASTER_SMOKE_ORDER`. Never read outside
        // that trailing #[cfg(test)] module.
        name: "VIKE_EVENTS_API_SMOKE_TOKEN",
        krate: "vike-backfill",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "",
    },
    // ⚠ TOMBSTONE — the `("VIKE_COUNTERS_FILE", "vike-desktop")` row stood here and is DELETED, not
    // re-keyed. The desktop cut took the local trading core out of the GUI (orders leave it only
    // through the backend), and the mmap health-counters pre-flight went with it: `counters_file_path`
    // was that read's only caller and lost its own. `vike_stat` and the daemon still read the same
    // variable — the `vike-core` row below is that one — so the VARIABLE is alive and only the GUI's
    // read is gone. This registry is keyed on `(name, krate)` and `krate_of` derives the key from the
    // FILE PATH, so a re-keyed `vike-desktop` row would have been a row nothing measures:
    // `every_declared_variable_is_read` fails on that exactly as hard as the missing-row direction.
    Setting {
        name: "VIKE_COUNTERS_FILE",
        krate: "vike-core",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "",
    },
    // The COMPUTE daemon's bind consent, and it deliberately REUSES the datahub's variable
    // rather than minting `VIKE_BACKTEST_ALLOW_PUBLIC_BIND` (ruling 7 of
    // `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`). The question it
    // answers is a property of the BOX — "may this box's node protocol be reached off-box" —
    // and one box now runs two daemons speaking it; a second variable would let an operator
    // believe they had answered it while the other daemon still refused. Read by
    // `crates/vike-backtest/src/backtest_cli.rs`'s `run_serve` off the swept map, and handed to
    // the SAME `vike_datahub_client::bind`'s `bind_decision` the data daemon calls.
    vike_map("VIKE_DATAHUB_ALLOW_PUBLIC_BIND", "vike-backtest", ""),
    Setting {
        // The NAMED-RUN lane's ARMING switch on the compute daemon
        // (`docs/decisions/0064-a-named-run-carries-no-source.md`'s decision 8). Default OFF: an
        // armed lane lets an OBSERVE-scope peer spend this box's CPU on a backtest, and it
        // PUBLISHES the operator's own compiled-in strategy names (that record's decision 7).
        //
        // ⚠ RUNTIME rather than a build feature, deliberately:
        // `docs/decisions/0035-the-image-ships-every-feature-and-may-be-the-primary-install.md`
        // means a feature gate would be ON by default exactly where it matters most.
        //
        // ⚠ `Injected`, and that is why it is a MAP read rather than an `env::var`.
        // `vike_backtest::named_run::NamedRunLane::from_vars` is a pure parser over the sweep
        // `backtest_cli`'s `run` already owns; a `Layer::Library` read would have joined
        // `crates/vike-ops/tests/settings/settings_registry.rs`'s `LIBRARY_PIN`, which is a RATCHET that may
        // only shrink — so the correct shape was also the only one CI would accept.
        //
        // ⚠ `MapLookup` even though the key IS a named constant
        // (`vike_backtest::named_run::NAMED_RUN_ENV`): `crates/vike-ops/src/settings/tests.rs`'s
        // `layer_and_naming_agree_on_every_row` holds that an `Injected` row is a map lookup,
        // because what that pairing describes is HOW THE VALUE IS REACHED, not how the key is
        // spelled. The constant stays anyway — it is the doc anchor the arming message and the
        // tests both name.
        name: "VIKE_BACKTEST_NAMED_RUN",
        krate: "vike-backtest",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "unset = DISARMED (the lane still ANSWERS both verbs; it runs nothing)",
    },
    // The per-user store fallback (`vike_model::paths::store_path`). Read wherever `VIKE_HIST_STORE` is,
    // and ONLY as the last step of the same precedence: they answer "where does a store go when this
    // machine is not the checkout that built the binary", which is a fresh INSTALL. Not `Scope::Vike`
    // — these are the platform's own variables, so the naming convention does not apply.
    //
    // ⚠ THREE rows where there used to be TWELVE. Each of vike-desktop, vike-datahub,
    // `vike_backtest::binutil` and `vike_backfill::cli` pasted the SAME three `std::env::var` lines
    // to build `user_data_dir`'s arguments — one identical computation, four chances to diverge,
    // and in the two bin-glue crates the paste sat in a LIBRARY file (six of the seventy-seven
    // `Layer::Library` rows). `vike_model::paths::store_path::user_data_dir_from_vars` takes the
    // already-collected environment MAP instead, so the trio is spelled in exactly ONE file and the
    // four callers pass `&std::env::vars().collect()` from their own binaries. The four crates'
    // rows are gone because those crates genuinely no longer name these variables — `krate` is
    // where the READ lives, and it lives here now.
    //
    // ⚠ `Layer::Injected`, not `Library`: `vike-model` never touches `std::env`. It is a pure
    // parser over a caller-supplied map — the STEP-2 TARGET state, which is why the six library
    // rows this replaces are retired rather than relocated.
    external_map("XDG_DATA_HOME", "vike-model", "<none> → $HOME/.local/share/vike-data"),
    external_map("HOME", "vike-model", "<none> → the repo default"),
    external_map("LOCALAPPDATA", "vike-model", "<none> → $HOME/vike-data (Windows only)"),
    // `vike_backtest::binutil::store_root` is now PURE — it reads this key out of the map its
    // four in-crate bins pass (`&std::env::vars().collect()`), so the read moved from a library
    // file to the binaries that always owned it. The lift came with the platform trio above: a
    // function cannot half-read the environment, and all four of this one's variables were on
    // the STEP-2 work-list together.
    vike_map("VIKE_HIST_STORE", "vike-backtest", "<repo>/market_data/hist"),
    // `backtest data fetch-starter` resolves `<project>/tmp` for the scratch its download passes
    // through, by the SAME walk that answers settings, user_data, data and bin — so one project has
    // one answer and this variable relocates all of them together. Read as a MAP LOOKUP from the
    // sweep the binary already owns, never from the process: this is library code under a
    // composition root that has the map.
    //
    // ⚠ The scratch may NOT go in the operating system's temp directory
    // (`crates/vike-ops/tests/hygiene/system_temp_gate.rs` refuses it): inside the container that path is
    // not the host's, does not survive a restart, and resolves somewhere else on an operator's box
    // — silently. That refusal is what put this read here.
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "vike-backtest",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "the project walk from the working directory — <project>/tmp, else the store root's parent",
    },
    // The twin of `VIKE_HIST_STORE`'s `vike-backtest` row: `vike_backfill::cli::store_root`, pure,
    // over the map its sixteen in-crate bin call sites pass.
    vike_map("VIKE_HIST_STORE", "vike-backfill", "<repo>/market_data/hist"),
    Setting {
        name: "VIKE_HOLD_STORE",
        krate: "vike-backtest",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_HOLD_TOKENS",
        krate: "vike-backtest",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_JOURNAL_DIR",
        krate: "vike-core",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Literal,
        default: "",
    },
    // ⚠ `Injected`, not `Binary`, since the multicall merge: the read moved out of
    // `src/bin/tearsheet.rs` into `crates/vike-report/src/tearsheet_cli.rs`'s `run`, which takes a
    // caller-supplied map. That is the direction this registry asks for — *libraries take
    // configuration as parameters; only binaries read the process environment* — so the move is the
    // registry's target state reached, not the LIBRARY_PIN ratchet worked around. The bin still
    // exists and still sweeps the environment; it is now the only thing in that crate that does.
    vike_map("VIKE_JOURNAL_DIR", "vike-report", ""),
    Setting {
        name: "VIKE_JOURNAL_SNAPSHOT_EVERY",
        krate: "vike-core",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Literal,
        default: "",
    },
    // The path to a COPY of a real `_manifest.json` that
    // `crates/vike-data/tests/store/manifest_delta.rs`'s `measure_v2_migration_on_a_real_manifest` runs
    // the v2 -> v3 migration against, to report what it costs in wall clock and in bytes. Ignored
    // by default and self-skipping when unset, because the case that matters is a 104 MB manifest
    // from the live data box and no such file is in this repository. ⚠ It must be a COPY: the test
    // WRITES where it points.
    Setting {
        name: "VIKE_MANIFEST_MIGRATION_FIXTURE",
        krate: "vike-data",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "unset — the measurement self-skips",
    },
    // ⚠ `VIKE_PACE_BOOK` (`vike-backfill`, `Injected`) stood here: the override for the one-shot
    // kline programs' persisted pace file. docs/decisions/0094 deleted the programs, the file and
    // its resolver together, so nothing reads the variable any more.
    // Baked in at compile time by `vike-strategy-plugin`'s own `build.rs`
    // (`cargo:rustc-env=VIKE_PLUGIN_FINGERPRINT=...`, folding TARGET/PROFILE/OPT_LEVEL/RUSTC
    // into one string), then read back via `env!("VIKE_PLUGIN_FINGERPRINT")` in
    // `src/fingerprint.rs` — the toolchain-fingerprint guard `loader::load` compares against a
    // plugin's own `vike_plugin_fingerprint()` export. `env!` is a COMPILE-TIME macro, not a
    // `vars.get(` call, but the scanner's loose literal sweep cannot tell the two apart (it has
    // no call syntax to anchor on for this shape), so this row records what it actually finds:
    // `Injected`/`MapLookup`, the same pairing the sweep gives any env-shaped literal it cannot
    // resolve to a direct `env::var` call.
    vike_map("VIKE_PLUGIN_FINGERPRINT", "vike-strategy-plugin", ""),
    Setting {
        name: "VIKE_PIN_CORES",
        krate: "vike-exec",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Konst("PIN_ENV"),
        default: "",
    },
    Setting {
        name: "VIKE_RECORD_CHAINS",
        krate: "vike-data",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Konst("RECORD_CHAINS_ENV"),
        default: "",
    },
    Setting {
        name: "VIKE_RECORD_CHAINS_CADENCE_MS",
        krate: "vike-data",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Konst("RECORD_CHAINS_CADENCE_ENV"),
        default: "60000",
    },
    Setting {
        name: "VIKE_RECORD_PROPERTIES",
        krate: "vike-data",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Konst("RECORD_PROPERTIES_ENV"),
        default: "",
    },
    Setting {
        // Regeneration switch for the var/zscore characterization pin
        // (`crates/vike-indicators/tests/window_pin.rs`). Unset = compare against the committed
        // fixture; "1" = rewrite it, which must be justified in the commit message.
        name: "VIKE_REGEN_WINDOW_PIN",
        krate: "vike-indicators",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Konst("REGEN_ENV"),
        default: "",
    },
    Setting {
        name: "VIKE_RUN_PROFILE",
        krate: "vike-core",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Literal,
        default: "",
    },
    // The SETTINGS DIRECTORY override — `<project>/settings` named outright, skipping the walk.
    //
    // The walk (`vike_model::paths::state_path::project_settings_dir`, which `vike_secrets::dotenv`
    // reaches by one-line delegation — ⚠ that read "its zero-`vike-*`-dependency twin" until
    // `docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md` (2026-09-20)
    // declared the edge and thereby collapsed the second spelling, so neither half of the phrase
    // survives; the store crate's property is its RANK, nothing above the vocabulary floor)
    // knows TWO project markers: a checkout's WORKSPACE ROOT (the
    // outermost `Cargo.toml` that declares a `[workspace]` table — neither the nearest manifest nor
    // the outermost one, both of which shipped and were bugs), else a DEPLOYMENT's own `settings/`
    // directory. The second marker exists because
    // all three shipped units run `WorkingDirectory=<project>`, where the install recipe puts a
    // BINARY and no source tree — a `Cargo.toml`-only walk returned `None` there, so a production
    // daemon loaded no policy, no config and NO CREDENTIALS, every venue silently on paper. This
    // variable is the escape hatch above both markers, for a layout neither describes.
    //
    // One row per crate that NAMES it — `vike-model`'s below, the rest in their own crates' family
    // files — in three different shapes:
    //   - `vike-model` / `vike-secrets` DECLARE it (`pub const SETTINGS_DIR_ENV`) and read nothing:
    //     both resolvers take the value as a PARAMETER, which is why neither joins `LIBRARY_PIN`.
    //     The raw-literal sweep still observes the constant, hence `Injected`.
    //   - `vike-bridge-core` pulls it out of the caller-supplied process-env map in
    //     `credentials::load_workspace_secrets_from_env` — the ONE lookup that gives all seven
    //     composition roots the hatch for their CREDENTIALS without any of them changing a line.
    //     Spelled as a LITERAL on purpose: `scan`'s map-lookup sweep resolves constants crate-wide,
    //     so importing `vike_secrets::SETTINGS_DIR_ENV` would make this read invisible here.
    //   - ⚠ HISTORICAL, and the live account is the tombstone on the `vike-boot` row
    //     (`crates/vike-ops/src/settings/rows/connections.rs`'s `VIKE_SETTINGS_DIR`): the four
    //     composition roots — the desktop shell (then `vike-app`), `vike-tradehub`, `vike-cli`,
    //     `vike-recorder` — each USED to look it up in its own `std::env::vars()` sweep, four rows
    //     in three different Layers. The first two placed the POLICY/config layer with it and read
    //     it in `main.rs` (`Binary`); `vike-cli`'s dispatcher lives in `src/lib.rs`, so its
    //     identical read scored `Injected` — the bin-adjacent-glue shape the registry's module doc
    //     already lists. `vike-recorder` loads no policy at all (it takes an explicit `--profile`)
    //     and used it for the other two things the directory answers: where the rolling trace log
    //     goes, and which credential store its silent-series pager reads. ⚠ Until 2026-08-08 that daemon read
    //     the variable NOWHERE — `strings` on the shipped binary found it zero times — while its
    //     unit set it and both runbooks claimed it made the answer independent of the working
    //     directory. It did not; resolution was 100% `WorkingDirectory=`.
    //
    // Unset (the default) changes nothing: the walk answers, exactly as it did before. A BLANK
    // value is ignored rather than honoured — it would otherwise resolve settings to `""` and read
    // credentials out of the working directory.
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "vike-model",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "<none> → the project walk: a checkout's workspace root, else a deployment's own settings/",
    },
    // ⚠ `vike-backfill` needs a `NON_READ_LITERAL_MENTIONS` entry to score `TestOnly` here: its
    // `src/cli.rs` spells the name in a fixture inside its `#[cfg(test)]` module (the file
    // `src/cli_tests.rs`), which the raw literal sweep reads as an `Injected` sighting and which
    // would then outrank the real reads in `tests/`. That entry says the fixture is a mention, not
    // a read.
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "vike-backfill",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    // `<project>/user_data` — the USER-CONTENT directory (strategies, run profiles, backtest
    // results, notebooks), and the SIBLING of the settings directory above rather than a child of
    // it. `crates/vike-model/src/paths/state_path.rs`'s `PROJECT_USER_DATA_DIR` argues the split: half of
    // `settings/` is machine-read and half machine-written, while a strategy somebody WROTE is
    // neither — and `settings/` holds live venue keys (the database under `settings/db`, or
    // `secrets.env` on a box that has not migrated), which must not ride along when a user copies
    // or commits their strategy library.
    //
    // A SEPARATE variable from `VIKE_SETTINGS_DIR`, deliberately: an operator relocating settings
    // for a deployment is answering a different question from a user pointing the app at a strategy
    // library on another disk, and one variable for both would force them to move together.
    //
    // The SHAPES (the `VIKE_USER_DATA_DIR` rows across the family files are the roster — no count
    // is written here):
    //   - `vike-model` DECLARES it (`pub const USER_DATA_DIR_ENV`) and reads nothing — the resolver
    //     takes the value as a PARAMETER, which is why it does not join `LIBRARY_PIN`. The
    //     raw-literal sweep still observes the constant, hence `Injected`.
    //   - every COMPOSITION ROOT that can compile a Rhai script looks it up in the
    //     `std::env::vars()` sweep it already performs, to find `user_data/indicators/` — the user's
    //     OWN indicators, which each root installs process-wide at startup so a script can call
    //     them. `vike-cli` additionally hands the directory to `cmd::init` to scaffold. Each is
    //     spelled as a LITERAL for the reason the `VIKE_SETTINGS_DIR` block above gives for the
    //     `vike-bridge-core` row: the map-lookup sweep resolves constants CRATE-wide, so importing
    //     `vike_model::paths::state_path::USER_DATA_DIR_ENV` would make the read invisible here. `Layer` is
    //     computed from the FILE PATH, which is why `vike-cli`'s is `Injected` (its dispatcher lives
    //     in `src/lib.rs`; `main.rs` is a one-line shim) while `vike-desktop`'s and
    //     `vike-datahub`'s —
    //     read in `main.rs` and `src/bin/` — are `Binary`.
    //
    // Unset (the default) changes nothing — the walk answers. A BLANK value is ignored rather than
    // honoured, or user content would resolve to `""` and strategies would be read out of the
    // working directory.
    Setting {
        name: "VIKE_USER_DATA_DIR",
        krate: "vike-model",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "<none> → <project>/user_data, off the same project walk as settings/",
    },
    // ⚠ `vike-datahub`'s row for this name is GONE (ruling 7 of
    // `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`). That daemon read it to
    // install the indicators a served `RunSlice` might call; the seven COMPUTE verbs — and with them
    // every Rhai compile — moved to `vike-backend backtest --addr`, so it installs nothing and reads
    // nothing. The read did not vanish, it MOVED: `crates/vike-backtest/src/compute_server.rs`'s
    // `install_user_indicators` is the same function, and the `vike-backtest` row below already
    // covered that crate.
    // The `backtest` bin's own read, and a row that was MISSING until direction 1 started demanding
    // one per `(name, krate)`: four crates already spelled the name — `vike-model` (above),
    // `vike-cli` (`crates/vike-ops/src/settings/rows/platform.rs`), `vike-desktop`
    // (`crates/vike-ops/src/settings/rows/gui.rs`) and a `vike-datahub` row since deleted (the
    // note above) — so the gate was satisfied
    // while this binary's read carried no `Layer`, no `Naming` and no default anywhere. It spells
    // the key as a LITERAL rather than importing `vike_model::paths::state_path::USER_DATA_DIR_ENV` on
    // purpose — the map-lookup sweep resolves constants CRATE-wide, so the import would make the
    // read invisible to the gate; the same trade `vike-cli`'s dispatcher makes for this variable.
    // ⚠ `Injected` since the multicall merge — the read moved from `src/bin/backtest.rs` into
    // `crates/vike-backtest/src/backtest_cli.rs`'s `run`, which takes the swept map as a parameter.
    // `Layer` is COMPUTED FROM THE PATH here, so this field follows the file rather than describing
    // an intent; `declared_layer_matches_the_path` is what says so. The read itself is unchanged —
    // it was already a map lookup, which is why only this one field moves.
    Setting {
        name: "VIKE_USER_DATA_DIR",
        krate: "vike-backtest",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "<none> → <project>/user_data (the user indicators a profile's script may call)",
    },
    Setting {
        name: "VIKE_USER_DATA_DIR",
        krate: "vike-user-strategies",
        scope: Scope::Vike,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        // The BUILD-TIME read: which user_data tree the compiled-strategy scan bakes into the
        // binary (`<workspace-root>/user_data` when unset — a fixed checkout hop, deliberately
        // NOT the runtime marker walk, because build scripts only ever run in a checkout).
        default: "<none> → <workspace-root>/user_data at BUILD time (compiled-strategy scan)",
    },
    Setting {
        name: "VIKE_USER_DATA_DIR",
        krate: "vike-user-research",
        scope: Scope::Vike,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        // The compiled-STUDY twin of the row above, and a SEPARATE row because these are keyed on
        // the `(name, krate)` PAIR: two build scripts read the same variable, each baking a
        // different tier of `user_data/` into the binary, and one row could only describe one of
        // them. Same resolution, deliberately — an operator who has learned the override for
        // strategies has learned it for studies.
        default: "<none> → <workspace-root>/user_data at BUILD time (compiled-study scan)",
    },
    Setting {
        name: "VIKE_SWEEP_SEQUENTIAL",
        krate: "vike-backtest",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Konst("SWEEP_SEQUENTIAL_ENV"),
        default: "Parallel",
    },
    Setting {
        name: "VIKE_SWEEP_THREADS",
        krate: "vike-backtest",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Konst("SWEEP_THREADS_ENV"),
        default: "min(4, available_parallelism())",
    },
    // Both read the SAME way, in the SAME crate: `crates/vike-strategy-plugin/tests/fixtures/
    // {good,bad_fingerprint,panicking}/build.rs`'s tiny `main` bakes each into its own fixture's
    // `env!()`, so `crates/vike-strategy-plugin/tests/load_refusals.rs` can build a fixture whose
    // handshake genuinely matches (or, for `bad_fingerprint`, deliberately does not) the real
    // crate's own `abi::ABI_VERSION`/`fingerprint::FINGERPRINT` — passed in from
    // `load_refusals.rs`'s own `build_stale_fixtures` via `.env("VIKE_TEST_ABI_VERSION", ..)`/
    // `.env("VIKE_TEST_FINGERPRINT", ..)`, never read from the ambient process environment.
    Setting {
        name: "VIKE_TEST_ABI_VERSION",
        krate: "vike-strategy-plugin",
        scope: Scope::Vike,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_TEST_FINGERPRINT",
        krate: "vike-strategy-plugin",
        scope: Scope::Vike,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        default: "",
    },
    // The builder service's own auth key (`vike_strategy_builder::builder::keys_from_vars`,
    // Task 6 of the runtime-loaded-Rust-strategies design). A pure map lookup over the
    // caller-supplied var map, never `std::env::var` — each of this service's composition
    // roots (`src/bin/vike-strategy-builder.rs`'s `main`, and since the multicall join
    // `vike-backend strategy-builder`) owns its own one `std::env::vars()` sweep and hands the
    // map to `builder::run`, which is where this lookup and its four siblings actually live.
    // ⚠ `krate` moved from "vike-strategy-plugin" to "vike-strategy-builder" when a controller
    // ruling split the builder service (this crate's own dependency tree) out of the
    // quarantined FFI surface, so a compiled plugin would stop inheriting it — see
    // `crates/vike-strategy-plugin/src/lib.rs`'s module doc. The env var itself is unchanged.
    // ⚠ Since 2026-09-26 the SERVICE takes its key from this variable OR from the file its
    // `_KEY_FILE` sibling below names, never both (`builder::key_source`); this is the VALUE
    // form, kept for the container image and read by `keys_from_vars` for every CLIENT.
    vike_map("VIKE_STRATEGY_BUILDER_KEY", "vike-strategy-builder", ""),
    // The FILE form of the builder service's key: a path whose contents are the key
    // (`vike_strategy_builder::builder::key_source`/`resolve_keys`, a literal `.get(` on the
    // caller-supplied map). What `deploy/vike-strategy-builder.service` sets, as
    // `%d/builder-key` beside a `LoadCredential=` line, so the key reaches the process as a
    // systemd credential FILE and never as a unit `Environment=` value — which any local user
    // can read over D-Bus. Setting both this and the value form refuses startup. The FILE is
    // opened by the composition root (`std::fs::read`, handed in), not by the library.
    vike_map("VIKE_STRATEGY_BUILDER_KEY_FILE", "vike-strategy-builder", ""),
    // The four lines below are all read the SAME way, and it moved once since it was first
    // written down here. They used to be read by `src/bin/vike-strategy-builder.rs`'s `main`
    // directly off its one `std::env::vars()` sweep (`Layer::Binary`, the shape
    // `crates/vike-ops/src/settings/tests.rs`'s `layer_and_naming_agree_on_every_row` doc names
    // for `vike-backfill/src/bin/tardis_backfill.rs`'s `TARDIS_API_KEY`).
    // ⚠ **Moved to `builder::run` (`builder.rs`), and the layer moved with it, when the
    // multicall join gave this service a SECOND composition root** — `vike-backend
    // strategy-builder` (`crates/vike/src/main.rs`'s `strategy_builder_main`) needed the same
    // reads, and the alternative was a second copy of every default and refusal message.
    // `builder::run` is a pure parser over a caller-supplied `&HashMap` (never
    // `std::env::var`), so all four are `Layer::Injected` now, the same shape
    // `VIKE_STRATEGY_BUILDER_KEY` already had — see that function's own doc.
    vike_map("VIKE_STRATEGY_BUILDER_PORT", "vike-strategy-builder", "7881"),
    vike_map("VIKE_STRATEGY_BUILDER_OUT_DIR", "vike-strategy-builder", "user_data/plugins"),
    vike_map("VIKE_STRATEGY_BUILDER_RETAIN", "vike-strategy-builder", "5"),
    // The checkout root a rendered plugin's Cargo.toml resolves its `vike-model`/
    // `vike-strategy-plugin` `path` dependencies against (`render::render_cargo_toml`,
    // threaded through `builder::serve`/`handle_connection`). Required, same shape and same
    // reason as `VIKE_STRATEGY_BUILDER_KEY` above: `crates/vike-ops/tests/hygiene/compile_time_path_gate.rs`
    // refuses a compile-time-baked fallback here (a running binary resolving a path from the
    // tree the COMPILER was built in, not the tree it runs from), and unlike a data path there
    // is no ladder to fall through to instead — a compiler either finds real crates to build
    // against or the compile fails, so `builder::run` refuses to start rather than guess.
    // ⚠ Moved to `Layer::Injected` with its three siblings above — see that comment.
    vike_map("VIKE_STRATEGY_BUILDER_WORKSPACE_ROOT", "vike-strategy-builder", ""),
];
