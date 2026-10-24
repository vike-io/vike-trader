//! The `SETTINGS` rows whose reading crate is a venue bridge.

use super::{venue_cred, venue_unread};
use crate::settings::{Layer, Medium, Naming, Scope, Setting};

pub(crate) const ROWS: &[Setting] = &[
    // ⚠ Decision 0156 item 6 deleted every venue ACCOUNT-credential row this file held: the tiered
    // names a bridge's loader composed or spelled to find its keys — `ALPACA_SANDBOX_*`,
    // `ASTER_TESTNET_*`, cTrader's per-tier token pair and its `CTRADER_DEMO_ACCOUNT_ID`,
    // `DUKASCOPY_DEMO1_*`/`DUKASCOPY_DEMO2_*`, `FXCM_DEMO_*`, `HYPERLIQUID_{DEMO,LIVE}*`,
    // `IBKR_{DEMO,LIVE}_*`, `IG_DEMO_*`, `OANDA_{SIM,DEMO,LIVE}_*` and the polymarket account's
    // `POLY_PRIVATE_KEY`, `POLY_FUNDER`, `POLY_RELAYER_API_KEY*` and `POLY_SIGNATURE_TYPE`. A
    // bridge now reads ITS ACCOUNT's credential by FIELD (`vike_secrets::account_credentials`), and a
    // field word is not a name looked up in a map, so no row describes one. cTrader's app pair is
    // the VENUE's fields `CLIENT_ID`/`CLIENT_SECRET` the same way; `CTRADER_CLIENT_ID` and
    // `CTRADER_CLIENT_SECRET` survive only as `vike-config`'s `Refused` rows, because
    // `ctrader_authorize` refuses the retired variables out of a table — a sighting, not a read, the
    // treatment its three sibling variables already had. Everything below is read by NAME: the
    // deployment rows (the builder-fee knobs, the JForex tools), the flags a root folds into the
    // map, the header and child-environment names, and the smokes' own process variables.
    venue_cred("ASTER_BUILDER_FEE_RATE", "bridges/aster", "0"),
    Setting {
        name: "ASTER_SMOKE_ORDER",
        krate: "bridges/aster",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "",
    },
    Setting {
        name: "CARGO_CFG_TARGET_OS",
        krate: "bridges/fxcm",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "",
    },
    Setting {
        name: "CARGO_FEATURE_FXCM",
        krate: "bridges/fxcm",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "",
    },
    Setting {
        name: "CARGO_MANIFEST_DIR",
        krate: "bridges/fxcm",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
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
        medium: Medium::ProcessEnv,
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
        medium: Medium::ProcessEnv,
        default: "cc",
    },
    // The three names the bridge WRITES into the JForex sidecar's environment
    // (`crates/bridges/dukascopy/src/exec.rs`'s `spawn_with_program`), filled from the mounted
    // account's `LOGIN`/`PASSWORD` fields. The Java child reads them; nothing in this crate does.
    venue_unread("DUKASCOPY_JNLP", "bridges/dukascopy", ""),
    venue_unread("DUKASCOPY_LOGIN", "bridges/dukascopy", ""),
    venue_unread("DUKASCOPY_PASSWORD", "bridges/dukascopy", ""),
    // The four process variables of the `#[ignore]`d dukascopy metal-amount smoke
    // (`crates/bridges/dukascopy/tests/dukascopy_live_smoke.rs`'s `dukascopy_jforex_metal_unit_far_limit`): how long
    // it holds the order for a person to read its amount, the largest notional (USD, at the live ask, one unit read
    // as one ounce) it will place an order for, the token that lets it place ONE real DEMO order, and the public
    // XAU/USD price it sits 30% under. Named like `DUKASCOPY_SMOKE_ACCOUNT` (a smoke's own variable,
    // `Scope::Venue`); the product reads none of them.
    Setting {
        name: "DUKASCOPY_METAL_HOLD_SECS",
        krate: "bridges/dukascopy",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "90",
    },
    Setting {
        name: "DUKASCOPY_METAL_MAX_NOTIONAL_USD",
        krate: "bridges/dukascopy",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "5000",
    },
    Setting {
        name: "DUKASCOPY_METAL_PROBE",
        krate: "bridges/dukascopy",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "",
    },
    Setting {
        name: "DUKASCOPY_METAL_REF_PX",
        krate: "bridges/dukascopy",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "",
    },
    Setting {
        name: "DUKASCOPY_SMOKE_ACCOUNT",
        krate: "bridges/dukascopy",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "",
    },
    Setting {
        name: "FCSDK_DIR",
        krate: "bridges/fxcm",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "",
    },
    // ⚠ Re-keyed from `vike-tradehub-core` when decision 0088's B3 step moved hyperliquid's mount arm
    // (including this read) into `bridges/hyperliquid`'s own `live_runtime_for_account` — the read
    // is per CRATE, not per name, so the row follows it rather than being left behind.
    venue_cred("HYPERLIQUID_BUILDER_FEE_TENTHS_BP", "bridges/hyperliquid", "0"),
    // The name the daemon folds `flags.hyperliquid_hip3` into the credential map under
    // (decision 0095 retired the variable): the instrument loader's `load_from_vars` reads it
    // out of that map.
    venue_cred("HYPERLIQUID_HIP3", "bridges/hyperliquid", ""),
    Setting {
        name: "JAVA_HOME",
        krate: "bridges/dukascopy",
        scope: Scope::External,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::CredentialMap,
        default: "<project>/bin/jre/*/bin/java, else `java` from PATH",
    },
    venue_cred("JFOREX_BRIDGE_JAR", "bridges/dukascopy", "<project>/bin/jforex/jforex-bridge.jar"),
    // `POLY_ADDRESS`, `POLY_API_KEY`, `POLY_NONCE`, `POLY_PASSPHRASE`, `POLY_SIGNATURE` and
    // `POLY_TIMESTAMP` are the HTTP HEADER names of Polymarket's L1/L2 authentication
    // (`crates/bridges/polymarket/src/exec_plane/auth.rs` and `l1.rs` write them), not settings:
    // the literal sweep sees each one, and no value is read under any of them.
    venue_unread("POLY_ADDRESS", "bridges/polymarket", ""),
    venue_unread("POLY_API_KEY", "bridges/polymarket", ""),
    Setting {
        name: "POLY_CANCEL_ORDER_ID",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "",
    },
    Setting {
        name: "POLY_CHAIN_FROM",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::MapLookup,
        medium: Medium::ProcessEnv,
        default: "",
    },
    Setting {
        name: "POLY_CHAIN_TO",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::MapLookup,
        medium: Medium::ProcessEnv,
        default: "",
    },
    venue_cred("POLY_EXEC", "bridges/polymarket", ""),
    Setting {
        name: "POLY_FILL_SMOKE",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "",
    },
    Setting {
        name: "POLY_GAMMA_SMOKE",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "",
    },
    // The escape hatch on the venue's own order-placement geoblock pre-flight
    // (`crates/bridges/polymarket/src/exec_plane/adapter.rs`'s `geoblock_override_enabled`). ⚠ `Injected`,
    // deliberately: the STEP-2 target shape is a caller-supplied map, and the `Layer::Library`
    // work-list is a ratchet that may shrink and never grow. (It was the odd one out among the
    // `POLY_EXEC*` gates until decision 0095 made its three siblings map-only too.) So this
    // flag is set in the credential store as a DEPLOYMENT row (by name, filed under no account),
    // never exported into the process env. It arms nothing — `flags.poly_exec` is still required, and a
    // `POLY_EXEC` line in that same store is read by nothing (the daemon writes the resolved
    // flag over it).
    venue_cred("POLY_GEOBLOCK_OVERRIDE", "bridges/polymarket", ""),
    venue_unread("POLY_NONCE", "bridges/polymarket", ""),
    venue_unread("POLY_PASSPHRASE", "bridges/polymarket", ""),
    venue_cred("POLY_RECONCILE", "bridges/polymarket", ""),
    Setting {
        name: "POLY_REDEEM_SMOKE",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "",
    },
    venue_unread("POLY_SIGNATURE", "bridges/polymarket", ""),
    venue_unread("POLY_TIMESTAMP", "bridges/polymarket", ""),
    Setting {
        name: "VIKE_CAPTURE_FIXTURES",
        krate: "bridges/binance",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "",
    },
    Setting {
        name: "VIKE_CAPTURE_FIXTURES",
        krate: "bridges/bybit",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
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
        medium: Medium::ProcessEnv,
        default: "",
    },
    Setting {
        name: "VIKE_SOAK_SECS",
        krate: "bridges/aster",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "900",
    },
    Setting {
        name: "VIKE_SOAK_SECS",
        krate: "bridges/binance",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "900",
    },
    Setting {
        name: "VIKE_SOAK_SECS",
        krate: "bridges/bybit",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "300",
    },
    Setting {
        name: "VIKE_SOAK_SECS",
        krate: "bridges/deribit",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "300",
    },
    Setting {
        name: "VIKE_SOAK_SECS",
        krate: "bridges/hyperliquid",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "300",
    },
    Setting {
        name: "VIKE_SOAK_SECS",
        krate: "bridges/okx",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "300",
    },
    // The `VIKE_SETTINGS_DIR` block on its `vike-model` row
    // (`crates/vike-config/src/settings/rows/research.rs`'s `VIKE_SETTINGS_DIR`) argues the override,
    // and its rows: one per crate that NAMES it, in three different shapes…
    // …plus one row per crate whose TESTS name it. A live venue smoke resolves the credential store
    // through the smoke helpers (`vike_secrets::smoke_directory` for the account it trades,
    // `vike_secrets::smoke_vars` for deployment names), which read the override in a
    // `test-support`-only file the registry scores `TestOnly`; a test that names the variable itself does so in the TEST BINARY
    // — itself a `main`, hence `Layer::TestOnly` rather than the `Layer::Library` a loader-side read would have added to `LIBRARY_PIN`'s
    // may-only-shrink work-list.
    //
    // ⚠ These rows exist because the smokes were UNRUNNABLE without them, not for symmetry.
    // `settings/` is gitignored, so a git worktree or a the CI box verification lane checks out
    // `settings/*.toml` and never `secrets.env`; every smoke called the override-blind loader,
    // resolved the empty `settings/` beside it and self-SKIPPED in silence — "no creds → stay
    // paper" is a legitimate state, so nothing was logged and nothing went red. Alpaca is the case
    // that forced it: its hosts are unreachable from the Windows dev box, so its reconcile smoke
    // can ONLY ever be proven from a lane, and until 2026-08-19 it could not be.
    //
    // ⚠ Nine bridge rows (alpaca, aster, binance, bybit, ctrader, deribit, hyperliquid, ig, okx)
    // LEFT with decision 0146: their smokes stopped naming the variable — they open the store through
    // the smoke helpers, which read it once for every smoke — so their crates read it nowhere. The rows below are the bridges whose tests still name it themselves.
    //
    // The roster is every `Layer::TestOnly` `VIKE_SETTINGS_DIR` row across the family files (the
    // bridge crates' are the rows below; the others sit in their own crates' family files) — one
    // per crate whose `tests/` name it, and no count is written here or anywhere else.
    // `every_declared_variable_is_read` fails on a row whose crate has stopped naming it, which is
    // what keeps the roster honest in the direction that rots.
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "bridges/dukascopy",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "bridges/fxcm",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "bridges/polymarket",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "bridges/vike-ibkr",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
];
