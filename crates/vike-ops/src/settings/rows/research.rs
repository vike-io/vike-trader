//! The `SETTINGS` rows read by the research, backtest, strategy, data and model crates.

use super::{external_env, vike_env};
use crate::settings::{Layer, Medium, Naming, Scope, Setting};

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
        medium: Medium::ProcessEnv,
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
    external_env("CARGO", "vike-strategy-builder", "cargo"),
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
        medium: Medium::ProcessEnv,
        default: "cargo",
    },
    Setting {
        name: "CARGO_MANIFEST_DIR",
        krate: "vike-user-strategies",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
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
        medium: Medium::ProcessEnv,
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
        medium: Medium::ProcessEnv,
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
        medium: Medium::ProcessEnv,
        default: "",
    },
    Setting {
        name: "EOD_SMOKE",
        krate: "vike-backfill",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "",
    },
    Setting {
        name: "OUT_DIR",
        krate: "vike-user-strategies",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
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
        medium: Medium::ProcessEnv,
        default: "",
    },
    Setting {
        name: "PMXT_SMOKE",
        krate: "vike-backfill",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
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
        medium: Medium::ProcessEnv,
        default: "",
    },
    Setting {
        name: "PROFILE",
        krate: "vike-strategy-plugin",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "",
    },
    Setting {
        name: "RUSTC",
        krate: "vike-strategy-plugin",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "rustc",
    },
    Setting {
        name: "TARGET",
        krate: "vike-strategy-plugin",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "",
    },
    Setting {
        name: "TARDIS_API_KEY",
        krate: "vike-backfill",
        scope: Scope::External,
        layer: Layer::Binary,
        naming: Naming::MapLookup,
        medium: Medium::CredentialMap,
        default: "",
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
        medium: Medium::CredentialMap,
        default: "",
    },
    Setting {
        // data.vike.io archive backfill (vike-backfill's `vike-archive` feature): the
        // `vike_archive_backfill` bin reads this from its scoped credential-store read,
        // `store.get("VIKE_ARCHIVE_API_KEY")` (the MapLookup idiom `TARDIS_API_KEY` also uses),
        // and passes it into `vike_archive::ArchiveClient::new` as a plain parameter — the library
        // itself never reads the environment or the store.
        name: "VIKE_ARCHIVE_API_KEY",
        krate: "vike-backfill",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::MapLookup,
        medium: Medium::CredentialMap,
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
        medium: Medium::ProcessEnv,
        default: "",
    },
    // ⚠ TOMBSTONE — `VIKE_COUNTERS_FILE` has no reader row left. The desktop's went with the desktop
    // cut (the GUI lost its local trading core and the mmap health-counters pre-flight with it), and
    // `vike-core`'s — `vike_stat`'s fallback for its path — went with decision 0111: the counters
    // file is that tool's one positional argument, and the variable refuses startup through its
    // `vike-config` row.
    // The per-user store fallback (`vike_model::paths::store_path`). Read wherever a store root is
    // resolved, and ONLY as the last step of the same precedence: they answer "where does a store go when this
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
    external_env("XDG_DATA_HOME", "vike-model", "<none> → $HOME/.local/share/vike-data"),
    external_env("HOME", "vike-model", "<none> → the repo default"),
    external_env("LOCALAPPDATA", "vike-model", "<none> → $HOME/vike-data (Windows only)"),
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
        medium: Medium::ProcessEnv,
        default: "the project walk from the working directory — <project>/tmp, else the store root's parent",
    },
    Setting {
        name: "VIKE_HOLD_STORE",
        krate: "vike-backtest",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "",
    },
    Setting {
        name: "VIKE_HOLD_TOKENS",
        krate: "vike-backtest",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "",
    },
    // `vike-core`'s two journal rows went with decision 0111: `vike_core::journal_config_from` takes
    // the directory and the snapshot cadence as PARAMETERS (the daemon's `config.journal_dir` and
    // `config.journal_snapshot_every` rows), and reads no map at all. `vike-report`'s row — the
    // tearsheet's fallback for `--journal` — went with P7: the flag alone names the directory.
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
    // resolve to a direct `env::var` call. `Medium::CompileTime` is what the row can say that those
    // two columns cannot: no running process reads this name.
    Setting {
        name: "VIKE_PLUGIN_FINGERPRINT",
        krate: "vike-strategy-plugin",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::CompileTime,
        default: "",
    },
    // `vike-data`'s three recorder rows (`VIKE_RECORD_CHAINS`, `VIKE_RECORD_CHAINS_CADENCE_MS`,
    // `VIKE_RECORD_PROPERTIES`) stood here until decision 0111's P7 deleted the recorders'
    // environment doors: each recorder takes its gate (and the chain recorder its cadence) as a
    // PARAMETER, and every one of the three variables refuses startup through `vike-config`.
    Setting {
        // Regeneration switch for the var/zscore characterization pin
        // (`crates/vike-indicators/tests/window_pin.rs`). Unset = compare against the committed
        // fixture; "1" = rewrite it, which must be justified in the commit message.
        name: "VIKE_REGEN_WINDOW_PIN",
        krate: "vike-indicators",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Konst("REGEN_ENV"),
        medium: Medium::ProcessEnv,
        default: "",
    },
    // The SETTINGS DIRECTORY override — `<project>/settings` named outright, skipping the walk.
    //
    // The walk (`vike_model::paths::state_path::project_settings_dir`, which `vike_secrets::store_locator`
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
        medium: Medium::ProcessEnv,
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
        medium: Medium::ProcessEnv,
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
        medium: Medium::ProcessEnv,
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
        medium: Medium::ProcessEnv,
        default: "<none> → <project>/user_data (the user indicators a profile's script may call)",
    },
    Setting {
        name: "VIKE_USER_DATA_DIR",
        krate: "vike-user-strategies",
        scope: Scope::Vike,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
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
        medium: Medium::ProcessEnv,
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
        medium: Medium::ProcessEnv,
        default: "Parallel",
    },
    // Both read the SAME way, in the SAME crate: `crates/vike-strategy-plugin/tests/fixtures/
    // {good,bad_fingerprint,panicking}/build.rs`'s tiny `main` bakes each into its own fixture's
    // `env!()`, so `crates/vike-strategy-plugin/tests/load_refusals.rs` can build a fixture whose
    // handshake genuinely matches (or, for `bad_fingerprint`, deliberately does not) the real
    // crate's own `abi::ABI_VERSION`/`fingerprint::FINGERPRINT` — passed in from
    // `load_refusals/cache.rs`'s own `build_stale_fixtures` via `.env("VIKE_TEST_ABI_VERSION", ..)`/
    // `.env("VIKE_TEST_FINGERPRINT", ..)`, never read from the ambient process environment.
    Setting {
        name: "VIKE_TEST_ABI_VERSION",
        krate: "vike-strategy-plugin",
        scope: Scope::Vike,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "",
    },
    Setting {
        name: "VIKE_TEST_FINGERPRINT",
        krate: "vike-strategy-plugin",
        scope: Scope::Vike,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
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
    vike_env("VIKE_STRATEGY_BUILDER_KEY", "vike-strategy-builder", ""),
    // The FILE form of the builder service's key: a path whose contents are the key
    // (`vike_strategy_builder::builder::key_source`/`resolve_keys`, a literal `.get(` on the
    // caller-supplied map). What `deploy/vike-strategy-builder.service` sets, as
    // `%d/builder-key` beside a `LoadCredential=` line, so the key reaches the process as a
    // systemd credential FILE and never as a unit `Environment=` value — which any local user
    // can read over D-Bus. Setting both this and the value form refuses startup. The FILE is
    // opened by the composition root (`std::fs::read`, handed in), not by the library.
    vike_env("VIKE_STRATEGY_BUILDER_KEY_FILE", "vike-strategy-builder", ""),
    // The builder's four plain settings — its port, artifact directory, retention and workspace
    // root — have no reader rows: decision 0111 made them the daemon's command-line flags
    // (`--port`, `--out-dir`, `--retain`, `--workspace-root` on its unit's `ExecStart=`), and
    // `vike-config` refuses each variable at startup.
    // The LIVE smoke's own two inputs (`crates/vike-strategy-builder/tests/builder_live_smoke.rs`): the
    // running service's `--out-dir` and `--port`, which a test cannot read off a running unit.
    Setting {
        name: "VIKE_BUILDER_SMOKE_OUT_DIR",
        krate: "vike-strategy-builder",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "required by the live smoke: the running builder's --out-dir",
    },
    Setting {
        name: "VIKE_BUILDER_SMOKE_PORT",
        krate: "vike-strategy-builder",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "the builder's default port, 7881",
    },
];
