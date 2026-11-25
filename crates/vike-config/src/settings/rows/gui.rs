//! The `SETTINGS` rows read by the desktop GUI crates.

use super::{external_credential, external_env};
use crate::settings::{Layer, Medium, Naming, Scope, Setting};

pub(crate) const ROWS: &[Setting] = &[
    // THE STUDIO CHAT PANE'S TWO AI-PROVIDER KEYS, looked up in the CREDENTIAL map the desktop
    // loads from the settings database and hands to `vike_studio::StudioChatApiKeys::resolve`
    // (`crates/vike-ai/src/provider.rs`'s `Provider::credential_key` names them, so the row is
    // `vike-ai`'s: the scanner finds the names there). `Injected`/`MapLookup`,
    // so `LIBRARY_PIN` does not grow: the pane reads no environment and no store of its own. These
    // are the rows that make `vike-cli secrets set ANTHROPIC_API_KEY` / `CEREBRAS_API_KEY` legal
    // (`vike_venues::credential_keys::STORE_KEYS_OUTSIDE_THE_GRID`, held to this table in both
    // directions). Absent is the ordinary state: the provider is not offered and Send stays disabled.
    // A PRESENT-but-BLANK value counts as present (see `StudioChatApiKeys`).
    //
    // ⚠ `ANTHROPIC_API_KEY` is ALSO a row of `vike-agent-evaluation` (a process-environment read in that
    // crate's two binaries, `crates/vike-config/src/settings/rows/research.rs`): a row is keyed on
    // `(name, krate)`, so the two readers stay two rows. The credential store answers THIS one only.
    external_credential("ANTHROPIC_API_KEY", "vike-ai", ""),
    external_credential("CEREBRAS_API_KEY", "vike-ai", ""),
    Setting {
        // A THIRD independent read of the same variable, for the same reason, in a third crate (this row
        // was `vike-studio-core`'s until the test moved with the run pipeline, 2026-11-10, then
        // `vike-backtesthub`'s until the run of a strategy file moved into the sandboxed child, plan 2b):
        // `crates/vike-backend/tests/support/strategy_file.rs`'s `build_the_artifact` hands the real builder the
        // cargo running the test, so the artifact the `vike-backend strategy-sandbox` child then loads
        // was built by the toolchain whose fingerprint that loader checks against. A row is keyed on
        // `(name, krate)` (the krate is the DIRECTORY, `crates/vike-backend`), which is why this is its own row
        // rather than an edit to either of the other two — the `vike-strategy-builder` and
        // `vike-rust-strategy` rows, `crates/vike-config/src/settings/rows/research.rs`'s `CARGO`.
        name: "CARGO",
        krate: "vike-backend",
        scope: Scope::External,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "cargo",
    },
    Setting {
        // A FOURTH, for the live verb's production route (plan 2b, F5):
        // `crates/vike-strategy-sandbox/tests/live_child.rs` hands the real builder the cargo running the
        // test, so the artifact its `strategy-sandbox live` child loads passes the loader's fingerprint.
        name: "CARGO",
        krate: "vike-strategy-sandbox",
        scope: Scope::External,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "cargo",
    },
    Setting {
        // A FIFTH, for CI's check of the SDK adapters (decision 0296's verdict 8):
        // `crates/vike-strategy-lab/tests/sdk_adapters.rs` hands the real builder the cargo running the test,
        // so each adapter's artifact passes this host's loader.
        name: "CARGO",
        krate: "vike-strategy-lab",
        scope: Scope::External,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
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
        medium: Medium::ProcessEnv,
        default: "",
    },
    Setting {
        name: "OUT_DIR",
        krate: "vike-desktop",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "",
    },
    Setting {
        name: "VIKE_APPMAX",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "",
    },
    Setting {
        name: "VIKE_ARRANGE",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "Grid",
    },
    Setting {
        name: "VIKE_CAL_PAGE",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "0",
    },
    Setting {
        // QA capture hook: draw the two `vike_app_core::ui::capture_seed::trendline_overlay` drawings
        // on every chart whose series has bars. PRESENT-ness is the whole value (`is_ok()`) —
        // hence the empty default, which is "not set ⇒ nothing drawn and every
        // `ChartState::drawings` map stays empty".
        name: "VIKE_CHART_DRAW",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Konst("CHART_DRAW_ENV"),
        medium: Medium::ProcessEnv,
        default: "",
    },
    // (The desktop's `VIKE_DATAHUB_{OBSERVE,CONTROL}_KEY` rows stood here until decision 0288's cutover:
    // the desktop signs every hub dial with this PC's device key, and `vike-app-core` names neither
    // variable any more. A set SHRINKING because its subject was deleted.)
    // A MEASUREMENT hook, not a feature: log the achieved frame rate once a second. It exists
    // because the same idle chart costs 10.9–12.5% CPU natively on a GPU and 124–275% under
    // lavapipe in a container, and varying the resolution ruled fill rate out — the cost is
    // per-FRAME, so the open question is how many frames each environment draws. Read ONCE, into the
    // desktop's `GuiEnv` at startup, rather than per frame: this is the hottest loop in the binary.
    Setting {
        name: "VIKE_FRAME_LOG",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Konst("FRAME_LOG_ENV"),
        medium: Medium::ProcessEnv,
        default: "unset (off) — the EXACT string \"1\" enables it",
    },
    // `("VIKE_HIST_STORE", "vike-studio")` — the `studio_seed` dev example's store root — stood here
    // until decision 0111's P7: the example takes the root as its positional argument.
    Setting {
        name: "VIKE_MAX",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "",
    },
    Setting {
        name: "VIKE_MIN",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "",
    },
    Setting {
        name: "VIKE_POLY_COCKPIT_TOKEN",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "POLY-DEMO",
    },
    Setting {
        // Regeneration switch for the tessellated-frame text goldens
        // (`crates/vike-chart/tests/headless/tessellation_goldens.rs`). Unset = compare against the
        // committed `tests/goldens/*.txt`; "1" = rewrite them, which must be justified in the
        // commit message. Same shape as `crates/vike-config/src/settings/rows/research.rs`'s
        // `VIKE_REGEN_WINDOW_PIN`, deliberately: one idiom for "rewrite the committed fixture", so
        // an operator who has met one has met both.
        name: "VIKE_REGEN_FRAME_GOLDENS",
        krate: "vike-chart",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Konst("REGEN_ENV"),
        medium: Medium::ProcessEnv,
        default: "",
    },
    Setting {
        // The SAME regeneration switch, read from the Studio shell's goldens twin
        // (`crates/vike-studio/tests/headless/tessellation_goldens.rs`). Rows are keyed on the
        // (name, krate) pair, so vike-chart's row above cannot declare this crate's read — one
        // more row, same `Konst` spelling, deliberately the same variable so one idiom rewrites
        // every frame-golden suite in the tree.
        name: "VIKE_REGEN_FRAME_GOLDENS",
        krate: "vike-studio",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Konst("REGEN_ENV"),
        medium: Medium::ProcessEnv,
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
        medium: Medium::ProcessEnv,
        default: "",
    },
    Setting {
        name: "VIKE_SCALE",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "",
    },
    Setting {
        name: "VIKE_SHOT",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "",
    },
    Setting {
        name: "VIKE_SHOT_FRAME",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "180",
    },
    Setting {
        name: "VIKE_SHOT_WIN",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "",
    },
    // The STATE ROOT override's two GUI rows (`vike-app-core`'s workspace family, `vike-studio`'s
    // `studio_workspace.json`) stood here until decision 0111: both read the directory the
    // desktop's boot resolved, declared once by the binary, and `VIKE_STATE_ROOT` refuses startup
    // (its `vike-config` row).
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
        medium: Medium::ProcessEnv,
        default: "<none> → the project walk from where the installer runs; no project found → the launcher carries none, and the installer says so",
    },
    // Settings > AI's Connect Claude (`crates/vike-app-core/src/ui/claude_connect.rs`'s
    // `find_claude_desktop`): where Claude Desktop keeps its config, looked up in the sweep the
    // desktop's `main` owns. The platform's own variables, so `Scope::External`.
    external_env("APPDATA", "vike-app-core", "<none> → no Claude Desktop on Windows"),
    external_env("HOME", "vike-app-core", "<none> → no Claude Desktop on macOS"),
    external_env(
        "LOCALAPPDATA",
        "vike-app-core",
        "<none> → the Store and classic installs are not looked for",
    ),
    Setting {
        name: "VIKE_USER_DATA_DIR",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::MapLookup,
        medium: Medium::ProcessEnv,
        // ONE lookup in this binary: `install_user_indicators` (the user's own indicators, into the
        // chart's ƒx picker) calls `state_path::user_data_dir_beside` with this variable off the one
        // `PROCESS_ENV` sweep and the boot's already-resolved settings directory as the sibling. The
        // second, `studio_study_host` (the Studio Research pane's studies + runs roots), was deleted
        // on 2026-11-19: studies run only on the backtest server, and the pane is its thin client.
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
        medium: Medium::ProcessEnv,
        default: "",
    },
    // (Studio's backtest key, the `VIKE_STUDIO_BACKTEST_KEY` deployment row Studio signed with until
    // decision 0288's cutover, has no row: Studio signs with this PC's device key, and nothing reads
    // that row any more.)
    Setting {
        name: "VIKE_STUDIO_TAB",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Konst("STUDIO_TAB_ENV"),
        medium: Medium::ProcessEnv,
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
    // licensed it — and `crates/vike-config/src/settings/rows/config.rs`'s `VIKE_TICK_STORE` is the
    // re-keyed `vike-config` twin that refusal owes the registry, the same shape its sibling
    // `VIKE_TRADEHUB_RECORD` takes.
    //
    // `vike_paths::tick_store_path::resolve_tick_store_root` — the ladder the deleted row
    // documented — still has no production caller, only its own unit tests. THAT question is the
    // one left open: it is dead code, or it is the resolver a future tick plane is written
    // against, and nothing here decides which.
    Setting {
        name: "VIKE_TOOL",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "",
    },
    Setting {
        name: "VIKE_TOOLS",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "",
    },
    // `("VIKE_TRADEHUB_CONTROL", "vike-app-core")` stood here until decision 0111: the desktop's
    // control master gate is the `flags.tradehub_control` row of the PC's own database, which the
    // binary passes in, and the variable refuses startup (its `vike-config` row).
    // (The `vike-app-core` rows for `VIKE_TRADEHUB_{OBSERVE,CONTROL}_KEY` stood here until decision 0288's
    // cutover, for the same reason as the datahub pair's above.)
    // The desktop's workspace-file override — a QA and development switch, not a setting, so
    // decision 0111 leaves it in the environment (`ENV_ALLOWLIST`'s `DesktopDevHarness`). Read by
    // `main.rs`'s `gui_env` and declared to `vike-app-core`'s workspace family, which read it itself
    // (a `Library` row) until 2026-11-15.
    Setting {
        name: "VIKE_WORKSPACE",
        krate: "vike-desktop",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "<project>/settings/state/workspace.json",
    },
];
