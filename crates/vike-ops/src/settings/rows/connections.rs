//! The `SETTINGS` rows read by `vike-connections`, `vike-secrets` and `vike-boot`.

use super::venue_map;
use crate::settings::{Layer, Naming, Scope, Setting};

pub(crate) const ROWS: &[Setting] = &[
    // The Connections editor's WRITE side for the same knob `bridges/aster` reads
    // (`crates/vike-ops/src/settings/rows/bridges.rs`'s `ASTER_BUILDER_FEE_RATE`):
    // `vike_connections::keys`'s `attribution_fields` spells this name as a literal so the LIVE
    // form can offer the field beside the builder CODE it pairs with. It writes and never reads —
    // the same shape the `DUKASCOPY_DEMO*` `vike-connections` rows carry.
    venue_map("ASTER_BUILDER_FEE_RATE", "vike-connections", "0"),
    venue_map("ASTER_LIVE_PRIVATE_KEY", "vike-connections", ""),
    venue_map("ASTER_LIVE_SIGNER", "vike-connections", ""),
    venue_map("ASTER_LIVE_USER", "vike-connections", ""),
    venue_map("ASTER_SIM_PRIVATE_KEY", "vike-connections", ""),
    venue_map("ASTER_SIM_USER", "vike-connections", ""),
    venue_map("ASTER_TESTNET_PRIVATE_KEY", "vike-connections", ""),
    venue_map("ASTER_TESTNET_SIGNER", "vike-connections", ""),
    venue_map("ASTER_TESTNET_USER", "vike-connections", ""),
    venue_map("BINANCE_LIVE_API_KEY", "vike-connections", ""),
    venue_map("BINANCE_LIVE_API_PASSPHRASE", "vike-connections", ""),
    venue_map("BINANCE_LIVE_API_SECRET", "vike-connections", ""),
    venue_map("DUKASCOPY_DEMO1_LOGIN", "vike-connections", ""),
    venue_map("DUKASCOPY_DEMO1_PASSWORD", "vike-connections", ""),
    venue_map("DUKASCOPY_DEMO2_LOGIN", "vike-connections", ""),
    venue_map("DUKASCOPY_DEMO2_PASSWORD", "vike-connections", ""),
    venue_map("FXCM_DEMO_PASSWORD", "vike-connections", ""),
    venue_map("FXCM_DEMO_USER", "vike-connections", ""),
    venue_map("FXCM_MAINNET_PASSWORD", "vike-connections", ""),
    venue_map("FXCM_MAINNET_USER", "vike-connections", ""),
    // The Connections editor's WRITE side — the twin of the `ASTER_BUILDER_FEE_RATE`
    // `vike-connections` row, and the reason the two knobs are spelled out rather than derived:
    // they are two different names with two different units, not one grammar.
    venue_map("HYPERLIQUID_BUILDER_FEE_TENTHS_BP", "vike-connections", "0"),
    venue_map("HYPERLIQUID_DEMO_PRIVATE_KEY", "vike-connections", ""),
    venue_map("HYPERLIQUID_LIVE_PRIVATE_KEY", "vike-connections", ""),
    venue_map("HYPERLIQUID_SIM_PRIVATE_KEY", "vike-connections", ""),
    venue_map("IG_DEMO_API_KEY", "vike-connections", ""),
    venue_map("IG_DEMO_IDENTIFIER", "vike-connections", ""),
    venue_map("IG_DEMO_PASSWORD", "vike-connections", ""),
    venue_map("OANDA_DEMO_ACCOUNT_ID", "vike-connections", ""),
    venue_map("OANDA_DEMO_API_KEY", "vike-connections", ""),
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "vike-secrets",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "<none> → the project walk: a checkout's workspace root, else a deployment's own settings/",
    },
    // ⚠ FOUR composition-root rows stood here — `vike-desktop`, `vike-tradehub`, `vike-cli`,
    // `vike-recorder` — and they are now ONE, under `vike-boot`. None of those binaries reads the
    // variable any more: each hands its `std::env::vars()` sweep to `vike_boot::boot`, which owns
    // the startup sequence and performs the project walk ONCE per process. The consolidation is the
    // point rather than a side effect — the walk happening in five places is what made the CI box's "no
    // policy, no credentials, every venue silently paper" expensive to fix, and what let
    // the desktop shell's rolling log file land under a different project from the block describing
    // it.
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "vike-boot",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "<none> → the project walk; the ONE walk per process, feeding the settings load, the log home, the credential store and the startup disclosure",
    },
];
