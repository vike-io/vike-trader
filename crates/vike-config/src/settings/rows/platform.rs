//! The `SETTINGS` rows read by logging, build identity, bridge core, the CLI and ops.

use super::{venue_cred, vike_cred, vike_env};
use crate::settings::{Layer, Medium, Naming, Scope, Setting};

pub(crate) const ROWS: &[Setting] = &[
    // A build script that reads it, and the row that was MISSING until direction 1 started
    // demanding a row per `(name, krate)` rather than per name — the two rows that already existed
    // spelled the name (`bridges/fxcm`'s, in `crates/vike-config/src/settings/rows/bridges.rs`, and
    // `vike-user-strategies`', in `crates/vike-config/src/settings/rows/research.rs`), so the gate was
    // satisfied while this crate's read had nothing recorded for it.
    // `vike-buildinfo`'s `build.rs` hops from the manifest directory to the repo root to shell out
    // to `git`, so an absent value is a hard `expect` rather than a fallback: cargo always sets it,
    // and a build script that could not locate its own crate has nothing to fall back TO.
    Setting {
        name: "CARGO_MANIFEST_DIR",
        krate: "vike-buildinfo",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "",
    },
    // The same convention, one crate over: `crates/vike-ops/tests/release/release_fxcm_artifact_gate.rs`
    // links its own probes to kill each of the release script's `verify` assertions — a correct
    // one, and three each missing one property — for the same reason the fxcm row exists (the
    // convention's source, `crates/vike-config/src/settings/rows/bridges.rs`'s `CC`),
    // that a rule keyed on a file MENTIONING a flag survives the emission of it being deleted.
    // Unset falls back to `cc`; a box with neither compiler skips the test loudly.
    Setting {
        name: "CC",
        krate: "vike-ops",
        scope: Scope::External,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "cc",
    },
    // -- vike-buildinfo's build script: the four cargo-supplied facts it stamps into the BUILD
    //    IDENTITY every shipped binary reports. None is an operator knob and none can be set from a
    //    settings file — cargo supplies all four to a build script, and by the time a binary runs
    //    they are compile-time constants. They have rows because `find_env_reads` resolves a direct
    //    `env::var` argument WITHOUT the prefix filter (deliberately — see `scan::ENV_PREFIXES`), so
    //    an undeclared one fails `every_read_variable_is_declared`. `BuildScript` is what the file
    //    path implies and what `declared_layer_matches_the_path` measures.
    Setting {
        name: "OUT_DIR",
        krate: "vike-buildinfo",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "",
    },
    // The OS search path, read to PREPEND a directory rather than to configure anything:
    // `crates/vike-ops/tests/release/release_fxcm_artifact_gate.rs` puts a fake `cargo` in front of the
    // real one so it can drive `scripts/release_fxcm_artifact.sh`'s whole `build` composition —
    // including a shim that CLOBBERS the default artifact, which is the only way to prove the
    // byte-identity guard is wired in rather than merely defined. The inherited value is kept as
    // the tail, so `bash`/`readelf`/`sha256sum` still resolve; unset degrades to the shim dir
    // alone, and the test then skips on the missing toolchain rather than lying.
    Setting {
        name: "PATH",
        krate: "vike-ops",
        scope: Scope::External,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "<inherited> → the shim directory is prepended to it",
    },
    // See the `OUT_DIR` note above: cargo-supplied build-script facts, not operator knobs.
    Setting {
        name: "PROFILE",
        krate: "vike-buildinfo",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "",
    },
    Setting {
        // Cargo's own `rustc`, which is not necessarily the one on `PATH` — the whole reason the
        // build script asks rather than shelling out to a bare `rustc`.
        name: "RUSTC",
        krate: "vike-buildinfo",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "rustc",
    },
    Setting {
        name: "RUST_LOG",
        krate: "vike-log",
        scope: Scope::External,
        layer: Layer::Library,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "info",
    },
    // The target triple `vike_buildinfo::TARGET` reports — see the `OUT_DIR` note above.
    Setting {
        name: "TARGET",
        krate: "vike-buildinfo",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "",
    },
    Setting {
        // ⚠ NOT a read. The only place this workspace spells the name is
        // `crates/vike-secrets/tests/credential_chain_roots.rs`, which plants a populated
        // credential file under a temp directory, names that directory with this variable, and
        // asserts the credentials come from the PROJECT store anyway. The row exists because the
        // literal sweep observes it there and `every_read_variable_is_declared` keys on the NAME, so
        // without one the gate reports an undeclared variable; `TestOnly` is what the file path
        // implies and what `declared_layer_matches_the_path` measures.
        //
        // The `vike-cli` sighting (`cmd/config.rs`'s redaction NEGATIVE table: an OS path is not a
        // `_USER` credential) is subtracted by `NON_READ_LITERAL_MENTIONS` and is not a row.
        //
        // `MapLookup`, not the `Literal` this row carried until `declared_naming_matches_the_call_site`
        // learned to check the field: the test names the variable as a KEY it inserts into the
        // caller-supplied map it then hands to `vike_secrets::open_credential_store`. Nothing in
        // `vike-secrets` reads it from process env at all, so `Literal` claimed a direct-read
        // spelling that exists nowhere in the crate — and since #1102 that claim is operator-facing,
        // where it would have read as "this comes from the environment" about the one variable this
        // test exists to prove INERT. Its three platform siblings (`HOME`/`XDG_DATA_HOME`/
        // `LOCALAPPDATA`, under `vike-model`) are `MapLookup` too. `Medium::NotRead` for the same
        // reason: the test plants the name precisely to show that no value is read under it.
        name: "USERPROFILE",
        krate: "vike-secrets",
        scope: Scope::External,
        layer: Layer::TestOnly,
        naming: Naming::MapLookup,
        medium: Medium::NotRead,
        default: "<never read — asserted inert>",
    },
    // The name the daemon folds `flags.allow_withdraw_keys` into the credential map under
    // (decision 0095 retired the variable); the binance withdraw gate reads it there.
    vike_cred("VIKE_ALLOW_WITHDRAW_KEYS", "vike-secrets", ""),
    // vike-buildinfo's CI-only freeze switch, read by its BUILD SCRIPT: the exact value `1` freezes
    // the build identity of a DEBUG build (no git probe, no git path watched), so a commit stops
    // recompiling every crate above that one. `.github/workflows/ci.yml` sets it for its whole run;
    // a RELEASE build ignores it, which is what keeps it off every shipped binary.
    // `crates/vike-buildinfo/build.rs`'s module doc is the argument, and
    // `xtask/tests/ci_plan_gate.rs` holds where it may be set. Not an operator knob: by
    // the time a binary runs, its effect is a compile-time constant.
    Setting {
        name: "VIKE_BUILDINFO_FREEZE",
        krate: "vike-buildinfo",
        scope: Scope::Vike,
        layer: Layer::BuildScript,
        naming: Naming::Konst("FREEZE_ENV"),
        medium: Medium::ProcessEnv,
        default: "",
    },
    Setting {
        name: "VIKE_LOG_DIR",
        krate: "vike-log",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        // `LogConfig::project_dir` (the binary's `<project>/settings/state/logs`) is what every
        // shipped bin now passes; `<exe_dir>/logs` remains the last resort for a binary with no
        // project above it. See `vike_log::resolve_log_dir`'s four-layer table.
        default: "<none> → <project>/settings/state/logs, else <exe_dir>/logs",
    },
    // `("VIKE_LOG_FILE_LEVEL", "vike-log")` stood here until decision 0111's verdict 3 was
    // finished: the file-log level is the `preferences.log_file_level` row every binary hands in,
    // and the variable refuses startup (its `vike-config` row).
    // `("VIKE_MAX_ORDER_QTY", "vike-cli")` stood here until decision 0111: the `trade`/`mcp` preview's
    // quantity cap is the `preferences.max_order_qty` row the boot installs, and the variable
    // refuses startup (its `vike-config` row).
    // `("VIKE_SETTINGS_DIR", "vike-bridge-core")` stood here until docs/decisions/0135: the
    // whole-map and account loaders that looked it up moved into `vike-secrets`, whose own row
    // (`rows/gateway_panel.rs`) already declares the read.
    // `("VIKE_CREDENTIAL_SCOPE", "vike-secrets")` stood here until decision 0146 renamed the
    // live-key fence and made it strict: the variable is a `vike_config::REMOVED_ENV` refusal now
    // (its `vike-config` row), so a fence left under its old name stops a booting process instead
    // of being ignored.
    //
    // The LIVE-KEY FENCE. It can only NARROW: unset or `false` is every credential the store holds,
    // `true` withholds every credential of an account whose ROW is at tier `live` (the tier the
    // operator wrote, never one read out of a name, decision 0156 item 6) plus the aster and
    // polymarket families, inside every query the `vike_secrets::CredentialStore` handle makes, and any other value (`1`, `yes`, `demo`, a
    // typo, a blank or padded word) is a `vike_secrets::ScopeError` that refuses startup. An env var
    // rather than a row on purpose: the fence belongs to ONE process, and the database is shared by
    // every process on the box. Two rows, one per crate that names it:
    //   - `vike-secrets` DECLARES it (`NO_LIVE_KEYS_ENV`, the const `CredentialScope::parse`'s
    //     error names) and reads it only in `vike_secrets::smoke_store`, a `test-support`-only file
    //     the registry scores `TestOnly` (`settings_registry.rs`'s `TEST_SUPPORT_SRC_FILES`); the
    //     literal sweep observes the constant, hence `Injected`.
    //   - `vike-boot` READS it, once per process, off the root's own environment sweep — in `boot`
    //     and in `open_credential_store` for a root that runs no boot.
    vike_env(
        "VIKE_NO_LIVE_KEYS",
        "vike-boot",
        "<none> → every credential the store holds; `true` → no credential of a `live` account, nor aster's or polymarket's; `false` → every credential; anything else → refused at startup",
    ),
    vike_env(
        "VIKE_NO_LIVE_KEYS",
        "vike-secrets",
        "<none> → every credential the store holds; `true` → no credential of a `live` account, nor aster's or polymarket's; `false` → every credential; anything else → refused (the root's startup refusal, the smoke helper's panic)",
    ),
    Setting {
        name: "VIKE_USER_DATA_DIR",
        krate: "vike-cli",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::ProcessEnv,
        default: "<none> → <project>/user_data (the directory `vike-cli init` scaffolds)",
    },
    // STEP 2, done: `cmd/trade.rs` and `cmd/mcp.rs` each read this with `std::env::var` — two
    // `Layer::Library` rows — and that was not merely a layering smell, it was the DEFECT. The
    // `vike-tradehub` daemon on the other end of the socket takes the same key out of the
    // CREDENTIAL STORE (the settings database, or `node.env` on a box that has not migrated),
    // which is never exported to process env, so a correctly-configured box got "not set in
    // the environment" and an exit. The dispatcher now owns both reads and `cmd/hubkeys.rs`
    // resolves them from a caller-supplied map: process env first, store second.
    vike_env("VIKE_TRADEHUB_CONTROL_KEY", "vike-cli", ""),
    // STEP 2, done — the observe twin of the `VIKE_TRADEHUB_CONTROL_KEY` row above; same two
    // library reads retired into one injected resolver (`cmd/hubkeys.rs`), same defect.
    vike_env("VIKE_TRADEHUB_OBSERVE_KEY", "vike-cli", ""),
    // `vike-cli secrets ibc-start` / `ibkr-cp-login` (2026-10-08) READ the IBKR gateway's login pair
    // out of the credential store — those two rows, by a scoped read — and launch the process that
    // logs in (`crates/vike-cli/src/cmd/secrets/ibkr_login.rs`'s `LOGIN_NAMES`). A gateway login is
    // a DEPLOYMENT name the launcher reads by name — not an account's field, and not in the
    // generated grid, which holds only the attribution codes — so these are declared by hand; being
    // declared is also what lets `vike-cli secrets set` WRITE them on a store that does not hold
    // them yet (`set`'s registry-reader rule).
    venue_cred("IBKR_DEMO_PASSWORD", "vike-cli", ""),
    venue_cred("IBKR_DEMO_USERNAME", "vike-cli", ""),
    // The LIVE (real-money) twin, read by `ibc-start --tier live` ONLY (`LIVE_LOGIN_NAMES` in the
    // same module), which starts a second IB Gateway for the LIVE account. `ibkr-cp-login` never
    // reads it: the Client Portal live tier has no fill path.
    venue_cred("IBKR_LIVE_PASSWORD", "vike-cli", ""),
    venue_cred("IBKR_LIVE_USERNAME", "vike-cli", ""),
];
