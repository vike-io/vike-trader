//! The `SETTINGS` rows whose reading crate is a venue bridge.

use super::venue_map;
use crate::settings::{Layer, Naming, Scope, Setting};

pub(crate) const ROWS: &[Setting] = &[
    venue_map("ALPACA_SANDBOX_ACCOUNT_ID", "bridges/alpaca", ""),
    venue_map("ALPACA_SANDBOX_CLIENT_ID", "bridges/alpaca", ""),
    venue_map("ALPACA_SANDBOX_CLIENT_SECRET", "bridges/alpaca", ""),
    venue_map("ASTER_BUILDER_FEE_RATE", "bridges/aster", "0"),
    Setting {
        name: "ASTER_SMOKE_ORDER",
        krate: "bridges/aster",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "",
    },
    venue_map("ASTER_TESTNET_PRIVATE_KEY", "bridges/aster", ""),
    venue_map("ASTER_TESTNET_USER", "bridges/aster", ""),
    Setting {
        name: "CARGO_CFG_TARGET_OS",
        krate: "bridges/fxcm",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "CARGO_FEATURE_FXCM",
        krate: "bridges/fxcm",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "CARGO_MANIFEST_DIR",
        krate: "bridges/fxcm",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        default: "",
    },
    // Where the fxcm build script writes `libfcshim.so` — the shared object that carries the C++
    // boundary to the ForexConnect SDK, and which `crates/bridges/fxcm/src/loader.rs` opens at
    // RUNTIME. New on 2026-09-09: before that the shim was a static archive linked into the binary
    // through `cc::Build::compile`, which owns `OUT_DIR` internally, so this script never named it.
    //
    // ⚠ It is also used to DERIVE the cargo profile directory (three `ancestors()` hops), because
    // cargo declares no variable for it and the loader's dev rung looks for the shim beside the
    // executable. That derivation is best-effort by construction — a failure is a `cargo:warning`,
    // not a build failure — since the installed and container shapes do not depend on it. `expect`
    // on the variable itself, though: cargo always sets it for a build script, and one that cannot
    // find its own output directory has nowhere to write.
    Setting {
        name: "OUT_DIR",
        krate: "bridges/fxcm",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        default: "",
    },
    // The C compiler `crates/bridges/fxcm/tests/fcsdk_rpath_tag.rs` drives to LINK a probe binary
    // and read back which rpath tag came out (`DT_RPATH` vs `DT_RUNPATH` — the distinction that
    // made the venue's packaged install unloadable). Honouring `$CC` is the ordinary convention and
    // is what lets the test follow a runner that does not put its compiler at `cc`; unset falls
    // back to `cc`, and a box with neither skips the test loudly rather than failing it.
    Setting {
        name: "CC",
        krate: "bridges/fxcm",
        scope: Scope::External,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "cc",
    },
    venue_map("CTRADER_CLIENT_ID", "bridges/ctrader", ""),
    venue_map("CTRADER_CLIENT_SECRET", "bridges/ctrader", ""),
    Setting {
        name: "CTRADER_DEMO_ACCESS_TOKEN",
        krate: "bridges/ctrader",
        scope: Scope::Venue,
        // `CtraderConfig::from_vars` reads this through a COMPUTED key
        // (`format!("CTRADER_{tier}_ACCESS_TOKEN")`) — an injected-map read the scanner cannot
        // resolve, so the row read `TestOnly` while the only literal sightings sat under `tests/`.
        // The catalog's injected-map test now spells it in `src`, making the observation match the
        // layer the variable has always really had.
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "CTRADER_DEMO_ACCOUNT_ID",
        krate: "bridges/ctrader",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "CTRADER_DEMO_REFRESH_TOKEN",
        krate: "bridges/ctrader",
        scope: Scope::Venue,
        // Same computed-key shape as `CTRADER_DEMO_ACCESS_TOKEN` above — see that row's note.
        // (`CTRADER_DEMO_ACCOUNT_ID` stays `TestOnly`: it is read the same injected way but is
        // still only literal-sighted under `tests/`, so the scanner cannot observe it here.)
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "CTRADER_LIVE_ACCESS_TOKEN",
        krate: "bridges/ctrader",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "CTRADER_LIVE_REFRESH_TOKEN",
        krate: "bridges/ctrader",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::MapLookup,
        default: "",
    },
    venue_map("DUKASCOPY_DEMO1_", "bridges/dukascopy", ""),
    venue_map("DUKASCOPY_DEMO1_LOGIN", "bridges/dukascopy", ""),
    venue_map("DUKASCOPY_DEMO1_PASSWORD", "bridges/dukascopy", ""),
    venue_map("DUKASCOPY_DEMO2_", "bridges/dukascopy", ""),
    venue_map("DUKASCOPY_DEMO2_LOGIN", "bridges/dukascopy", ""),
    venue_map("DUKASCOPY_DEMO2_PASSWORD", "bridges/dukascopy", ""),
    venue_map("DUKASCOPY_JNLP", "bridges/dukascopy", ""),
    venue_map("DUKASCOPY_LOGIN", "bridges/dukascopy", ""),
    venue_map("DUKASCOPY_PASSWORD", "bridges/dukascopy", ""),
    Setting {
        name: "DUKASCOPY_SMOKE_ACCOUNT",
        krate: "bridges/dukascopy",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "FCSDK_DIR",
        krate: "bridges/fxcm",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        default: "",
    },
    venue_map("FXCM_DEMO_PASSWORD", "bridges/fxcm", ""),
    venue_map("FXCM_DEMO_USER", "bridges/fxcm", ""),
    venue_map("FXCM_MAINNET_PASSWORD", "bridges/fxcm", ""),
    venue_map("FXCM_MAINNET_USER", "bridges/fxcm", ""),
    // ⚠ Re-keyed from `vike-mount` when decision 0088's B3 step moved hyperliquid's mount arm
    // (including this read) into `bridges/hyperliquid`'s own `live_mount_for_account` — the read
    // is per CRATE, not per name, so the row follows it rather than being left behind.
    venue_map("HYPERLIQUID_BUILDER_FEE_TENTHS_BP", "bridges/hyperliquid", "0"),
    venue_map("HYPERLIQUID_DEMO", "bridges/hyperliquid", ""),
    venue_map("HYPERLIQUID_DEMO_ACCOUNT_ADDRESS", "bridges/hyperliquid", ""),
    venue_map("HYPERLIQUID_DEMO_PRIVATE_KEY", "bridges/hyperliquid", ""),
    // The name the daemon folds `flags.hyperliquid_hip3` into the credential map under
    // (decision 0095 retired the variable): the instrument loader's `load_from_vars` reads it
    // out of that map.
    venue_map("HYPERLIQUID_HIP3", "bridges/hyperliquid", ""),
    venue_map("HYPERLIQUID_LIVE", "bridges/hyperliquid", ""),
    // The idle-cadence soak (#873) resolves the account address it watches from the `.env` map,
    // falling back to the LIVE key when no demo one is set — a `vars.get` in `tests/`, so
    // TestOnly + MapLookup.
    Setting {
        name: "HYPERLIQUID_LIVE_ACCOUNT_ADDRESS",
        krate: "bridges/hyperliquid",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::MapLookup,
        default: "",
    },
    venue_map("HYPERLIQUID_LIVE_PRIVATE_KEY", "bridges/hyperliquid", ""),
    venue_map("IBKR_DEMO_ACCOUNT", "bridges/vike-ibkr", ""),
    venue_map("IBKR_DEMO_CLIENT_ID", "bridges/vike-ibkr", ""),
    venue_map("IBKR_DEMO_DATA_CLIENT_ID", "bridges/vike-ibkr", ""),
    venue_map("IBKR_DEMO_MKTDATA_TYPE", "bridges/vike-ibkr", ""),
    venue_map("IBKR_LIVE_ACCOUNT", "bridges/vike-ibkr", ""),
    venue_map("IG_DEMO_API_KEY", "bridges/ig", ""),
    venue_map("IG_DEMO_IDENTIFIER", "bridges/ig", ""),
    venue_map("IG_DEMO_PASSWORD", "bridges/ig", ""),
    Setting {
        name: "JAVA_HOME",
        krate: "bridges/dukascopy",
        scope: Scope::External,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "<project>/bin/jre/*/bin/java, else `java` from PATH",
    },
    venue_map("JFOREX_BRIDGE_JAR", "bridges/dukascopy", "<project>/bin/jforex/jforex-bridge.jar"),
    venue_map("OANDA_DEMO_ACCOUNT_ID", "bridges/oanda", ""),
    venue_map("OANDA_DEMO_API_KEY", "bridges/oanda", ""),
    Setting {
        name: "OANDA_LIVE_ACCOUNT_ID",
        krate: "bridges/oanda",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OANDA_LIVE_API_KEY",
        krate: "bridges/oanda",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OANDA_MAINNET_ACCOUNT_ID",
        krate: "bridges/oanda",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OANDA_MAINNET_API_KEY",
        krate: "bridges/oanda",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OANDA_SIM_ACCOUNT_ID",
        krate: "bridges/oanda",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OANDA_SIM_API_KEY",
        krate: "bridges/oanda",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::MapLookup,
        default: "",
    },
    venue_map("POLY_ADDRESS", "bridges/polymarket", ""),
    venue_map("POLY_API_KEY", "bridges/polymarket", ""),
    Setting {
        name: "POLY_CANCEL_ORDER_ID",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "POLY_CHAIN_FROM",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "POLY_CHAIN_TO",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::MapLookup,
        default: "",
    },
    venue_map("POLY_EXEC", "bridges/polymarket", ""),
    Setting {
        name: "POLY_FILL_SMOKE",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "",
    },
    venue_map("POLY_FUNDER", "bridges/polymarket", ""),
    Setting {
        name: "POLY_GAMMA_SMOKE",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "",
    },
    // The escape hatch on the venue's own order-placement geoblock pre-flight
    // (`crates/bridges/polymarket/src/exec_plane/mount.rs`'s `geoblock_override_enabled`). ⚠ `Injected`,
    // deliberately: the STEP-2 target shape is a caller-supplied map, and the `Layer::Library`
    // work-list is a ratchet that may shrink and never grow. (It was the odd one out among the
    // `POLY_EXEC*` gates until decision 0095 made its three siblings map-only too.) So this
    // flag is set in the credential store beside the venue's other keys, never exported into
    // the process env. It arms nothing — `flags.poly_exec` is still required, and a
    // `POLY_EXEC` line in that same store is still refused by
    // `crates/vike-config/src/arming.rs`'s `refuse_credential_file_arming`.
    venue_map("POLY_GEOBLOCK_OVERRIDE", "bridges/polymarket", ""),
    venue_map("POLY_MAINNET_ADDRESS", "bridges/polymarket", ""),
    venue_map("POLY_MAINNET_PRIVATE_KEY", "bridges/polymarket", ""),
    venue_map("POLY_MAINNET_SECRET", "bridges/polymarket", ""),
    venue_map("POLY_NONCE", "bridges/polymarket", ""),
    venue_map("POLY_PASSPHRASE", "bridges/polymarket", ""),
    venue_map("POLY_PRIVATE_KEY", "bridges/polymarket", ""),
    venue_map("POLY_RECONCILE", "bridges/polymarket", ""),
    Setting {
        name: "POLY_REDEEM_SMOKE",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "",
    },
    venue_map("POLY_RELAYER_API_KEY", "bridges/polymarket", ""),
    venue_map("POLY_RELAYER_API_KEY_ADDRESS", "bridges/polymarket", ""),
    venue_map("POLY_SIGNATURE", "bridges/polymarket", ""),
    venue_map("POLY_SIGNATURE_TYPE", "bridges/polymarket", "Poly1271"),
    venue_map("POLY_TIMESTAMP", "bridges/polymarket", ""),
    Setting {
        name: "VIKE_CAPTURE_FIXTURES",
        krate: "bridges/binance",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_CAPTURE_FIXTURES",
        krate: "bridges/bybit",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "",
    },
    // The user-data idle-cadence soaks (#868/#873/#875) — one `#[ignore]`d, credential-gated
    // long-read measurement per venue, each reading the soak duration straight off process env.
    // Six rows because the table is keyed on (name, krate) and six bridge crates read it; the
    // default differs per venue (the two binance soaks want a window longer than the ~180s server
    // ping period, the rest settle for 300s).
    Setting {
        // Safety interlock for the `#[ignore]`d aster user-data soak. ⚠ This comment used to say
        // "aster is MAINNET-ONLY (no testnet endpoint)". That is FALSE and was corrected across the
        // tree on 2026-07-29 — this third copy was missed, which is exactly the rot the correction
        // record on `crates/bridges/aster/CLAUDE.md` exists to stop. The testnet endpoints are real
        // and routed to; only the TESTNET CREDENTIALS are unconfigured, so the soak would otherwise
        // fall through to the live account — hence the interlock.
        // Named through `const ALLOW_MAINNET_ENV` in `tests/aster_userdata_soak.rs`.
        name: "ASTER_SOAK_ALLOW_MAINNET",
        krate: "bridges/aster",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::Konst("ALLOW_MAINNET_ENV"),
        default: "",
    },
    Setting {
        name: "VIKE_SOAK_SECS",
        krate: "bridges/aster",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "900",
    },
    Setting {
        name: "VIKE_SOAK_SECS",
        krate: "bridges/binance",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "900",
    },
    Setting {
        name: "VIKE_SOAK_SECS",
        krate: "bridges/bybit",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "300",
    },
    Setting {
        name: "VIKE_SOAK_SECS",
        krate: "bridges/deribit",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "300",
    },
    Setting {
        name: "VIKE_SOAK_SECS",
        krate: "bridges/hyperliquid",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "300",
    },
    Setting {
        name: "VIKE_SOAK_SECS",
        krate: "bridges/okx",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "300",
    },
    // The `VIKE_SETTINGS_DIR` block on its `vike-model` row
    // (`crates/vike-ops/src/settings/rows/research.rs`'s `VIKE_SETTINGS_DIR`) argues the override,
    // and its rows: one per crate that NAMES it, in three different shapes…
    // …plus one row per crate whose TESTS name it. A live venue smoke resolves the credential store
    // through `vike_secrets::load_workspace_dotenv_from`, whose override is a PARAMETER, so the
    // read happens in the TEST BINARY — itself a `main`, hence `Layer::TestOnly` rather than the
    // `Layer::Library` a loader-side read would have added to `LIBRARY_PIN`'s may-only-shrink
    // work-list.
    //
    // ⚠ These rows exist because the smokes were UNRUNNABLE without them, not for symmetry.
    // `settings/` is gitignored, so a git worktree or a the CI box verification lane checks out
    // `settings/*.toml` and never `secrets.env`; every smoke called the override-blind loader,
    // resolved the empty `settings/` beside it and self-SKIPPED in silence — "no creds → stay
    // paper" is a legitimate state, so nothing was logged and nothing went red. Alpaca is the case
    // that forced it: its hosts are unreachable from the Windows dev box, so its reconcile smoke
    // can ONLY ever be proven from a lane, and until 2026-08-19 it could not be.
    //
    // The roster is every `Layer::TestOnly` `VIKE_SETTINGS_DIR` row across the family files (the
    // bridge crates' are the rows below; the others sit in their own crates' family files) — one
    // per crate whose `tests/` name it, and no count is written here or anywhere else.
    // `every_declared_variable_is_read` fails on a row whose crate has stopped naming it, which is
    // what keeps the roster honest in the direction that rots.
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "bridges/alpaca",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "bridges/aster",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "bridges/binance",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "bridges/bybit",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "bridges/ctrader",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "bridges/deribit",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "bridges/dukascopy",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "bridges/fxcm",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "bridges/hyperliquid",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "bridges/ig",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "bridges/okx",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "bridges/polymarket",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "bridges/vike-ibkr",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
];
