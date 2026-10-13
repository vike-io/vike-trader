//! The `SETTINGS` rows read by `vike-connections`, `vike-secrets` and `vike-boot`.

use super::venue_cred;
use crate::settings::{Layer, Medium, Naming, Scope, Setting};

pub(crate) const ROWS: &[Setting] = &[
    // The Connections editor's WRITE side for the same knob `bridges/aster` reads
    // (`crates/vike-config/src/settings/rows/bridges.rs`'s `ASTER_BUILDER_FEE_RATE`):
    // `vike_connections::keys`'s `attribution_fields` spells this name as a literal so the LIVE
    // form can offer the field beside the builder CODE it pairs with. It writes and never reads.
    //
    // ⚠ The editor's tiered credential-NAME rows (`ASTER_{LIVE,SIM,TESTNET}_*`, `BINANCE_LIVE_API_*`,
    // `DUKASCOPY_DEMO1_*`/`DUKASCOPY_DEMO2_*`, `FXCM_DEMO_*`, `HYPERLIQUID_{SIM,DEMO,LIVE}_PRIVATE_KEY`,
    // `IG_DEMO_*`, `OANDA_DEMO_*`) left with decision 0156 item 6: the editor writes an account's
    // credential by FIELD (`vike_secrets::save_owned_credentials`), so it spells no such name.
    venue_cred("ASTER_BUILDER_FEE_RATE", "vike-connections", "0"),
    // The Connections editor's WRITE side — the twin of the `ASTER_BUILDER_FEE_RATE`
    // `vike-connections` row, and the reason the two knobs are spelled out rather than derived:
    // they are two different names with two different units, not one grammar.
    venue_cred("HYPERLIQUID_BUILDER_FEE_TENTHS_BP", "vike-connections", "0"),
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "vike-secrets",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::ProcessEnv,
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
        medium: Medium::ProcessEnv,
        default: "<none> → the project walk; the ONE walk per process, feeding the settings load, the log home, the credential store and the startup disclosure",
    },
];
