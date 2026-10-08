//! The `SETTINGS` rows read by the desktop GUI crates.

use super::{external_map, vike_map};
use crate::settings::{Layer, Naming, Scope, Setting};

pub(crate) const ROWS: &[Setting] = &[
    // THE STUDIO CHAT PANE'S TWO AI-PROVIDER KEYS, looked up in the CREDENTIAL map the desktop
    // loads from the settings database and hands to `vike_studio::ChatApiKeys::resolve`
    // (`crates/vike-studio/src/panes/chat.rs`'s `key_env_var` names them). `Injected`/`MapLookup`,
    // so `LIBRARY_PIN` does not grow: the pane reads no environment and no store of its own. These
    // are the rows that make `vike-cli secrets set ANTHROPIC_API_KEY` / `CEREBRAS_API_KEY` legal
    // (`vike_model::credential_keys::STORE_KEYS_OUTSIDE_THE_GRID`, held to this table in both
    // directions). Absent is the ordinary state: the provider is not offered and Send stays disabled.
    // A PRESENT-but-BLANK value counts as present (see `ChatApiKeys`).
    //
    // ⚠ `ANTHROPIC_API_KEY` is ALSO a row of `vike-agent-eval` (a process-environment read in that
    // crate's two binaries, `crates/vike-ops/src/settings/rows/research.rs`): a row is keyed on
    // `(name, krate)`, so the two readers stay two rows. The credential store answers THIS one only.
    external_map("ANTHROPIC_API_KEY", "vike-studio", ""),
    external_map("CEREBRAS_API_KEY", "vike-studio", ""),
    Setting {
        // A THIRD independent read of the same variable, for the same reason, in a third crate:
        // `crates/vike-studio-core/tests/plugin_run.rs` hands the real builder the cargo running
        // the test, so the artifact it then loads through the PRODUCTION `build_strategy` was
        // built by the toolchain whose fingerprint that loader checks against. A row is keyed on
        // `(name, krate)`, which is why this is its own row rather than an edit to either of the
        // other two — the `vike-strategy-builder` and `vike-strategy-plugin` rows,
        // `crates/vike-ops/src/settings/rows/research.rs`'s `CARGO`.
        name: "CARGO",
        krate: "vike-studio-core",
        scope: Scope::External,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "cargo",
    },
    // The desktop's build script (`crates/vike-desktop/build.rs`) embeds the icon resource only when
    // cross-building the shipped `.exe`: `TARGET` picks the arm, `OUT_DIR` holds the resource script
    // and the object windres writes.
    Setting {
        name: "TARGET",
        krate: "vike-desktop",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "OUT_DIR",
        krate: "vike-desktop",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_APPMAX",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_ARRANGE",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "Grid",
    },
    Setting {
        name: "VIKE_CAL_PAGE",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "0",
    },
    Setting {
        // QA capture hook: draw the two `vike_app_core::ui::capture_seed::trendline_overlay` drawings
        // on every chart whose series has bars. PRESENT-ness is the whole value (`is_ok()`), the
        // `VIKE_TRADE_SEED` idiom — hence the empty default, which is "not set ⇒ nothing drawn
        // and every `ChartState::overlays` map stays empty".
        name: "VIKE_CHART_DRAW",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Konst("CHART_DRAW_ENV"),
        default: "",
    },
    // The DESKTOP's row for the same name as `vike-node-proto`'s
    // (`crates/vike-ops/src/settings/rows/daemons.rs`'s `VIKE_DATAHUB_OBSERVE_KEY`), and it is a
    // SECOND row because `SETTINGS` is keyed
    // on the PAIR `(name, krate)`: `crates/vike-app-core/src/backend/backend_registry.rs` declares its
    // OWN `DATAHUB_OBSERVE_KEY_NAME` constant rather than importing the client's, for the
    // reason its `OBSERVE_KEY_NAME` sibling gives — the scanner resolves a name through a
    // constant declared in THAT crate and no further, so an import would pass this registry by
    // BLINDNESS rather than by declaration. A `#[cfg(test)]` equality assert holds the two
    // spellings identical.
    //
    // It is what the desktop's MARKET-DATA plane signs with: `datahub_observe_keys` resolves it
    // process-environment-first, then `<project>/settings/node.env` through
    // `vike_secrets::resolve_node_keys` (decision 0051 — a PLATFORM key, never the venue
    // store). `Injected`, not `Library`: the settings directory and the environment map both
    // arrive as PARAMETERS from the binary's one boot walk and one `std::env::vars()` sweep, so
    // this read grows no `LIBRARY_PIN` row.
    //
    // ⚠ There is deliberately NO `VIKE_DATAHUB_CONTROL_KEY` row for this crate, and none for
    // `vike-desktop`: no dial this crate resolves — the store, the chart seed, the catalog,
    // the market-data session, the named run — may hold the datahub's control scope, which
    // COMPILES CLIENT-SUPPLIED RHAI and, on the data plane, backfills and deletes. ⚠ This read
    // "that key must never be resolved here" until 2026-09-26, and the rule narrowed rather
    // than broke: the owner ruled (`docs/decisions/0083`, question 1) that the DESKTOP resolves
    // it for Studio's compute dial alone, and it does so under ANOTHER name, in ANOTHER crate —
    // the `VIKE_STUDIO_COMPUTE_KEY` / `vike-studio` row. The platform name still has no reader
    // in any desktop crate, which is what this absence records.
    vike_map("VIKE_DATAHUB_OBSERVE_KEY", "vike-app-core", ""),
    Setting {
        name: "VIKE_EXPORT_DIR",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "<exe_dir>/exports",
    },
    // A MEASUREMENT hook, not a feature: log the achieved frame rate once a second. It exists
    // because the same idle chart costs 10.9–12.5% CPU natively on a GPU and 124–275% under
    // lavapipe in a container, and varying the resolution ruled fill rate out — the cost is
    // per-FRAME, so the open question is how many frames each environment draws. Read ONCE into a
    // `OnceLock` rather than per frame: this is the hottest loop in the binary.
    Setting {
        name: "VIKE_FRAME_LOG",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Konst("FRAME_LOG_ENV"),
        default: "unset (off) — the EXACT string \"1\" enables it",
    },
    Setting {
        name: "VIKE_HIST_STORE",
        krate: "vike-studio",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "market_data/hist",
    },
    Setting {
        name: "VIKE_JOURNAL_DIR",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_MAX",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_MIN",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_POLY_COCKPIT_TOKEN",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "POLY-DEMO",
    },
    Setting {
        // Regeneration switch for the tessellated-frame text goldens
        // (`crates/vike-chart/tests/tessellation_goldens.rs`). Unset = compare against the
        // committed `tests/goldens/*.txt`; "1" = rewrite them, which must be justified in the
        // commit message. Same shape as `crates/vike-ops/src/settings/rows/research.rs`'s
        // `VIKE_REGEN_WINDOW_PIN`, deliberately: one idiom for "rewrite the committed fixture", so
        // an operator who has met one has met both.
        name: "VIKE_REGEN_FRAME_GOLDENS",
        krate: "vike-chart",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Konst("REGEN_ENV"),
        default: "",
    },
    Setting {
        // The SAME regeneration switch, read from the Studio shell's goldens twin
        // (`crates/vike-studio/tests/tessellation_goldens.rs`). Rows are keyed on the
        // (name, krate) pair, so vike-chart's row above cannot declare this crate's read — one
        // more row, same `Konst` spelling, deliberately the same variable so one idiom rewrites
        // every frame-golden suite in the tree.
        name: "VIKE_REGEN_FRAME_GOLDENS",
        krate: "vike-studio",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Konst("REGEN_ENV"),
        default: "",
    },
    Setting {
        // Regeneration switch for the brand files' drift gate
        // (`crates/vike-ui-theme/tests/brand_assets.rs`). Unset = compare `assets/brand/` against
        // what `vike_ui_theme::brand` draws; "1" = rewrite the files that differ, which must be
        // justified in the commit message. The same idiom as `VIKE_REGEN_FRAME_GOLDENS` above.
        name: "VIKE_REGEN_BRAND_ASSETS",
        krate: "vike-ui-theme",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Konst("REGEN_ENV"),
        default: "",
    },
    Setting {
        name: "VIKE_SCALE",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_SHOT",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_SHOT_FRAME",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "180",
    },
    Setting {
        name: "VIKE_SHOT_WIN",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "",
    },
    // The STATE ROOT override (settings-unification Phase 2): one directory for every file the
    // program writes and no human edits — `workspace.json`, `studio_workspace.json`,
    // `alerts.json`. `vike_model::paths::state_path` is the pure resolver; these two rows and
    // `vike-tradehub`'s (`crates/vike-ops/src/settings/rows/daemons.rs`'s `VIKE_STATE_ROOT`) are
    // its three env-reading callers (see the platform-trio block,
    // `crates/vike-ops/src/settings/rows/research.rs`'s `XDG_DATA_HOME`, for the Layer argument).
    Setting {
        name: "VIKE_STATE_ROOT",
        krate: "vike-app-core",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Literal,
        default: "<none> → <project>/settings/state",
    },
    Setting {
        name: "VIKE_STATE_ROOT",
        krate: "vike-studio",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Literal,
        default: "<none> → <project>/settings/state",
    },
    // `vike-desktop --install-desktop-entry` (`crates/vike-app-core/src/ui/desktop_entry.rs`'s
    // `settings_dir`): the installer resolves the settings directory the app's own boot WOULD, off
    // the sweep `vike-desktop`'s `main` hands it, and writes it into the launcher's `Exec=` — a
    // launcher starts the app in the user's HOME, where the walk finds no project. It runs INSTEAD
    // of the boot (the flag exits first), so it is not a second answer beside `vike-boot`'s.
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "vike-app-core",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "<none> → the project walk from where the installer runs; no project found → the launcher carries none, and the installer says so",
    },
    Setting {
        name: "VIKE_USER_DATA_DIR",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::MapLookup,
        // TWO lookups in this binary, deliberately ONE resolution: `install_user_indicators` (the
        // user's own indicators, into the chart's ƒx picker) and `studio_study_host` (the Studio
        // Research pane's studies + runs roots) both call `state_path::user_data_dir_beside` with
        // the same two arguments — this variable off the one `PROCESS_ENV` sweep, and the boot's
        // already-resolved settings directory as the sibling. One row, because rows are keyed on
        // `(name, krate)` and a second would be a duplicate rather than extra coverage; one
        // resolution, because two could disagree and a study the Research pane can open but the
        // picker cannot see is indistinguishable from a file that failed to compile.
        //
        // ⚠ The THIRD consumer is gone: the same library used to be bound into the Rhai STRATEGY
        // namespace as well, reachable only in a build with a local trading core. The desktop cut
        // removed that core, so nothing in this process can call a strategy indicator; the read and
        // its resolution are unchanged, and only what the answer feeds got smaller.
        default: "<none> → <project>/user_data (the user's own indicators: ƒx-picker chart studies, \
                  plus the Studio Research pane's studies/runs roots)",
    },
    Setting {
        // Both VIKE_STUDIO_* rows moved crate AND layer in STEP 2: `StudioState::new` read them
        // inside `vike-studio`'s library, so a caller could neither see nor override the tab and
        // autorun a stale dev-shell export forced on it. They are `new_with_qa` PARAMETERS now, and
        // the ONE caller that wants them — `vike-desktop`'s `main.rs`, which mounts the Studio tool —
        // does the reads. Nothing else in the workspace wants them: the `studio_shot` capture
        // example poses `right_tab` on the struct directly, and every unit test calls the env-free
        // `new`. `krate_of` keys on the source path, so the rows had to move with the read.
        name: "VIKE_STUDIO_AUTORUN",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Konst("STUDIO_AUTORUN_ENV"),
        default: "",
    },
    // STUDIO'S COMPUTE KEY — the datahub CONTROL key, arriving under a name of its own
    // (`docs/decisions/0083-the-runtime-plugin-join-lands.md`, question 1: the owner's option
    // (a), "the desktop resolves the datahub Control key for Studio's COMPUTE dial only").
    // Read by `crates/vike-studio/src/backend/remote.rs`'s `compute_key_from_vars`, a pure lookup on
    // the map the desktop's ONE `std::env::vars()` sweep hands it — `Injected`/`MapLookup`,
    // the shape `VIKE_STRATEGY_BUILDER_KEY`'s row has, so `LIBRARY_PIN` does not grow.
    //
    // ⚠ Why a NEW name rather than a second reader of `VIKE_DATAHUB_CONTROL_KEY`: the desktop's
    // sweep is handed WHOLE to every datahub dial it resolves, and every process it starts
    // inherits it, so under the platform name one `node_keys_from_vars(env)` anywhere would
    // pick the Write key up without naming it. A name only this row's reader knows is the
    // reach limit; `crates/vike-ops/tests/gui/studio_compute_key_reach_gate.rs` holds it to one
    // Rust reader. The launcher (`scripts/studio.ps1`) is what sets it — the environment is
    // the ONLY rung, there is no store fallback. Unset (the default) is the ordinary state:
    // Studio's Run is then refused by a keyed compute daemon, naming this variable.
    vike_map("VIKE_STUDIO_COMPUTE_KEY", "vike-studio", ""),
    Setting {
        name: "VIKE_STUDIO_TAB",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Konst("STUDIO_TAB_ENV"),
        default: "",
    },
    // ⚠ TOMBSTONE — the `VIKE_TICK_STORE` rows are GONE, both of them, and the variable is read by
    // NOTHING in this workspace now. It was a PAIR: the desktop's row went with the GUI's local tick
    // plane (no tick recorder, no properties recorder, no chain recorder, no materializer), leaving
    // the daemon's as the only one, and the daemon's went with `record-feeds`/`materialize` under
    // `docs/decisions/0084-only-the-datahub-touches-the-store.md` — the reader it named was
    // `tick_store_root`, deleted in that same change.
    //
    // ⚠ **ANSWERED, not left open.** An operator with `VIKE_TICK_STORE` set believed it directed
    // where ticks land; nothing reads it, so it would configure nothing SILENTLY. It now REFUSES
    // the startup instead — `vike_config::REMOVED_ENV` carries the row and the measurement that
    // licensed it — and `crates/vike-ops/src/settings/rows/config.rs`'s `VIKE_TICK_STORE` is the
    // re-keyed `vike-config` twin that refusal owes the registry, the same shape its sibling
    // `VIKE_TRADEHUB_RECORD` takes.
    //
    // `vike_model::paths::tick_store_path::resolve_tick_store_root` — the ladder the deleted row
    // documented — still has no production caller, only its own unit tests. THAT question is the
    // one left open: it is dead code, or it is the resolver a future tick plane is written
    // against, and nothing here decides which.
    Setting {
        name: "VIKE_TOOL",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_TOOLS",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        // QA capture hook: seed the Trade panel with one resting PAPER limit order and one open
        // PAPER position. PRESENT-ness is the value (`is_ok()`), the idiom of the deleted `VIKE_DOM_TESTORDER`.
        //
        // ⚠ ONE row for TWO reads, and that is the registry's own rule rather than an omission:
        // rows are keyed on the PAIR `(name, krate)`, and both reads are in `vike-desktop`'s `main.rs`
        // — one arms `App::trade_seed_pending` (the frame loop submits the orders), one fills
        // `startup::StartupEnv::trade_seed` (the layout opens the bar feed that clocks the paper
        // fill). Two rows here would be a duplicate, not extra coverage.
        name: "VIKE_TRADE_SEED",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Konst("TRADE_SEED_ENV"),
        default: "",
    },
    Setting {
        name: "VIKE_TRADEHUB_CONTROL",
        krate: "vike-app-core",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Literal,
        default: "",
    },
    // Split-plane B1: the `--observe` observer's key NAMES moved out of `vike-desktop`'s
    // `main.rs` into `vike-app-core`'s `backend_conn::cli_observe_record` (the synthetic CLI
    // backend record), so the read is now `backend_registry::resolve_keys` over the
    // caller-supplied credentials map — the Injected shape. The binary still owns loading
    // that map.
    vike_map("VIKE_TRADEHUB_CONTROL_KEY", "vike-app-core", ""),
    // Split-plane B1 — the observe twin of the `VIKE_TRADEHUB_CONTROL_KEY` `vike-app-core`
    // row above: the name now lives in `backend_conn::cli_observe_record`, resolved by
    // `backend_registry::resolve_keys` over the caller-supplied map.
    vike_map("VIKE_TRADEHUB_OBSERVE_KEY", "vike-app-core", ""),
    Setting {
        name: "VIKE_WORKSPACE",
        krate: "vike-app-core",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Literal,
        default: "<project>/settings/state/workspace.json",
    },
];
