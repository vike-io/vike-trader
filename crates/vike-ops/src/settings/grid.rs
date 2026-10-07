//! `settings_grid` — THE GENERATED KEY GRID: the second table of the settings registry.
//!
//! [`crate::settings::SETTINGS`] holds every hand-written row. The rows here are the family it
//! structurally could not see — the credential and attribution-code keys
//! `vike_bridge_core::credentials` COMPOSES with a `format!` and then asks the credential map for,
//! which appear as a string literal at no read site. They are one family with one argument and
//! whole screens of identical rows, so they live in a file of their own rather than as a wall at the
//! bottom of the other table. The rows, the argument for them and the gate that checks them
//! (`crates/vike-ops/tests/settings/settings_registry.rs`'s `every_generated_key_is_declared`) did not
//! change with the move; only the file did.
//!
//! ⚠ **Two tables, one registry, and ONE way to iterate it.** [`crate::settings::all_settings`]
//! chains [`crate::settings::SETTINGS`] and the table below, and every consumer of the registry
//! reads that. A reader of `SETTINGS` alone has silently dropped every `Scope::Venue` credential key
//! — `vike-cli config show` would stop listing them and the Connections editor's completeness gate
//! would stop demanding them — with nothing failing to compile. The table is `pub(crate)` for that
//! reason: this module is reachable only through `all_settings`.
//!
//! The new-venue scaffold's row marker at the bottom of the table moved with it
//! (`crates/vike-ops/tests/venues/new_venue_gate/site_rules.rs`'s `STRUCTURAL_SITES` is keyed on this file).

use crate::settings::{Layer, Naming, Scope, Setting};

/// One row of `GENERATED_KEY_GRID`. The rows differ in nothing but `name`, so the five fields they
/// share are spelled ONCE, here, and the table below is one call per key. Why the crate is
/// `vike-bridge-core` and why the default is empty is argued at the head of the table.
const fn key(name: &'static str) -> Setting {
    Setting {
        name,
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    }
}

/// The generated credential/attribution key rows, sorted. Read through
/// [`crate::settings::all_settings`], never alone and never from outside this crate.
pub(crate) const GENERATED_KEY_GRID: &[Setting] = &[
    // -----------------------------------------------------------------------------------------
    // THE GENERATED KEY GRID — the family this table structurally could not see.
    //
    // Every row below is a key `vike_bridge_core::credentials` BUILDS with a `format!` and then
    // asks the credential map for. None of them appears as a string literal at its read site, so
    // `crates/vike-ops/src/scan.rs`'s `find_map_lookups` could not observe one and this table could
    // not declare one: `OKX_LIVE_API_SECRET` was read on every credential probe with neither a row
    // nor a sighting, while its `OKX_DEMO_API_SECRET` sibling had a row only because a test fixture
    // happened to spell it. That was the last STRUCTURAL hole in the registry, and it sat on the
    // credential path. `DYNAMIC_ALLOWLIST` could never have covered it — that table allowlists a
    // call site the scanner FOUND and could not resolve, and a computed map `get` is not recognised
    // as a candidate site at all.
    //
    // These rows are HAND-WRITTEN DATA, deliberately, and the gate that checks them derives its
    // expectation from somewhere else entirely: `vike_model::credential_keys`' `lookup_keys` folds
    // the suffix/tier tables over `vike_model::VENUES`, and
    // `crates/vike-ops/tests/settings/settings_registry.rs`'s `every_generated_key_is_declared` demands a row
    // here for each. GENERATING these rows from that enumeration would make the gate compare the
    // table to itself — the `roster_matches_the_bridge_crates` failure this repo has already shipped
    // once, green through a mutation that added a bridge crate. So adding a venue to the roster, a
    // tier, or a suffix reddens CI until this block is extended, which is the whole point.
    //
    // ⚠ `just new-venue NAME` DOES extend it, from the new-venue ROW MARKER at the bottom of this
    // block (spelling that token here would make this paragraph one), and that is NOT the
    // self-comparison the paragraph above refuses. The marker is
    // hand-written text in THIS file that spells four tiers and three suffixes literally; it cannot
    // see `credential_keys` and does not grow when that module does. Add a suffix or a tier there
    // and `every_generated_key_is_declared` goes red exactly as before — for every venue at once,
    // scaffolded or not. What the marker removes is only the case it can actually answer: a venue
    // joining the roster, whose twelve rows are pure mechanical restatement of the grid, and which
    // before this went red on the FIRST scaffold run with no generated row and no note anywhere —
    // the scavenger hunt `crates/vike-ops/tests/venues/new_venue_gate.rs` exists to end.
    //
    // Kept as ONE contiguous sorted block, in a file of its own, rather than interleaved into
    // `SETTINGS`' alphabetical body: it is a family with one shared argument, and scattering it
    // would bury that argument in three hundred unrelated rows. `vike_ops::settings::all_settings`
    // chains the two tables and `vike-cli config show` sorts its own output, so the operator view is
    // unaffected by the position.
    //
    // Every row is ONE call to `key`, the constructor above this table: the rows differ in nothing
    // but `name`, so the five fields they share are spelled once, there, and no row can drift from
    // them. ⚠ Until 2026-10-05 every row was spelled in full instead, deliberately, so that a grep
    // for the `Layer` field would count the grid's rows; it now finds the constructor's one line.
    // The grid's size is written down nowhere: `every_generated_key_is_declared` demands a row for
    // every key the enumeration composes, and `every_declared_variable_is_read` refuses a row
    // nothing reads.
    //
    // ⚠ The grid OVER-approximates over the roster, and `vike_model::credential_keys`' module doc
    // argues why (which venues reach the generic loader is a `match` arm in
    // `crates/vike-connections/src/status/arms.rs`'s `venue_env_configured`, not enumerable data) and
    // what it deliberately excludes (the BESPOKE per-venue shapes — `FXCM_{TIER}_USER`,
    // `DUKASCOPY_DEMO1_LOGIN`, the `POLY_*` trio — which are literals their own bridge's `config.rs`
    // spells, so the scanner always saw them and they always had rows).
    //
    // ⚠ `krate` is `vike-bridge-core` because that is where the map `get` happens, whatever crate
    // calls the loader — and the gate derives the SAME crate from the path of the file that composes
    // the keys. If that file ever moves crates, `every_generated_key_is_declared` and
    // `every_declared_variable_is_read` go red TOGETHER: that signature means RE-KEY these rows, not
    // delete them.
    //
    // ⚠ `default: ""` is not a placeholder. Unset means the venue stays PAPER; that absence IS the
    // live gate, and it is the most load-bearing behaviour in `credentials.rs`.
    // -----------------------------------------------------------------------------------------
    key("ALPACA_DEMO_API_KEY"),
    key("ALPACA_DEMO_API_PASSPHRASE"),
    key("ALPACA_DEMO_API_SECRET"),
    key("ALPACA_LIVE_API_KEY"),
    key("ALPACA_LIVE_API_PASSPHRASE"),
    key("ALPACA_LIVE_API_SECRET"),
    key("ALPACA_MAINNET_API_KEY"),
    key("ALPACA_MAINNET_API_PASSPHRASE"),
    key("ALPACA_MAINNET_API_SECRET"),
    key("ALPACA_SIM_API_KEY"),
    key("ALPACA_SIM_API_PASSPHRASE"),
    key("ALPACA_SIM_API_SECRET"),
    key("ASTER_BROKER_CODE"),
    key("ASTER_BUILDER_CODE"),
    key("ASTER_DEMO_API_KEY"),
    key("ASTER_DEMO_API_PASSPHRASE"),
    key("ASTER_DEMO_API_SECRET"),
    key("ASTER_LIVE_API_KEY"),
    key("ASTER_LIVE_API_PASSPHRASE"),
    key("ASTER_LIVE_API_SECRET"),
    key("ASTER_MAINNET_API_KEY"),
    key("ASTER_MAINNET_API_PASSPHRASE"),
    key("ASTER_MAINNET_API_SECRET"),
    key("ASTER_SIM_API_KEY"),
    key("ASTER_SIM_API_PASSPHRASE"),
    key("ASTER_SIM_API_SECRET"),
    key("BINANCE_BROKER_CODE"),
    key("BINANCE_BUILDER_CODE"),
    key("BINANCE_DEMO_API_KEY"),
    key("BINANCE_DEMO_API_PASSPHRASE"),
    key("BINANCE_DEMO_API_SECRET"),
    key("BINANCE_LIVE_API_KEY"),
    key("BINANCE_LIVE_API_PASSPHRASE"),
    key("BINANCE_LIVE_API_SECRET"),
    key("BINANCE_MAINNET_API_KEY"),
    key("BINANCE_MAINNET_API_PASSPHRASE"),
    key("BINANCE_MAINNET_API_SECRET"),
    key("BINANCE_SIM_API_KEY"),
    key("BINANCE_SIM_API_PASSPHRASE"),
    key("BINANCE_SIM_API_SECRET"),
    key("BYBIT_BROKER_CODE"),
    key("BYBIT_BUILDER_CODE"),
    key("BYBIT_DEMO_API_KEY"),
    key("BYBIT_DEMO_API_PASSPHRASE"),
    key("BYBIT_DEMO_API_SECRET"),
    key("BYBIT_LIVE_API_KEY"),
    key("BYBIT_LIVE_API_PASSPHRASE"),
    key("BYBIT_LIVE_API_SECRET"),
    key("BYBIT_MAINNET_API_KEY"),
    key("BYBIT_MAINNET_API_PASSPHRASE"),
    key("BYBIT_MAINNET_API_SECRET"),
    key("BYBIT_SIM_API_KEY"),
    key("BYBIT_SIM_API_PASSPHRASE"),
    key("BYBIT_SIM_API_SECRET"),
    key("CTRADER_DEMO_API_KEY"),
    key("CTRADER_DEMO_API_PASSPHRASE"),
    key("CTRADER_DEMO_API_SECRET"),
    key("CTRADER_LIVE_API_KEY"),
    key("CTRADER_LIVE_API_PASSPHRASE"),
    key("CTRADER_LIVE_API_SECRET"),
    key("CTRADER_MAINNET_API_KEY"),
    key("CTRADER_MAINNET_API_PASSPHRASE"),
    key("CTRADER_MAINNET_API_SECRET"),
    key("CTRADER_SIM_API_KEY"),
    key("CTRADER_SIM_API_PASSPHRASE"),
    key("CTRADER_SIM_API_SECRET"),
    key("DERIBIT_DEMO_API_KEY"),
    key("DERIBIT_DEMO_API_PASSPHRASE"),
    key("DERIBIT_DEMO_API_SECRET"),
    key("DERIBIT_LIVE_API_KEY"),
    key("DERIBIT_LIVE_API_PASSPHRASE"),
    key("DERIBIT_LIVE_API_SECRET"),
    key("DERIBIT_MAINNET_API_KEY"),
    key("DERIBIT_MAINNET_API_PASSPHRASE"),
    key("DERIBIT_MAINNET_API_SECRET"),
    key("DERIBIT_SIM_API_KEY"),
    key("DERIBIT_SIM_API_PASSPHRASE"),
    key("DERIBIT_SIM_API_SECRET"),
    key("DUKASCOPY_DEMO_API_KEY"),
    key("DUKASCOPY_DEMO_API_PASSPHRASE"),
    key("DUKASCOPY_DEMO_API_SECRET"),
    key("DUKASCOPY_LIVE_API_KEY"),
    key("DUKASCOPY_LIVE_API_PASSPHRASE"),
    key("DUKASCOPY_LIVE_API_SECRET"),
    key("DUKASCOPY_MAINNET_API_KEY"),
    key("DUKASCOPY_MAINNET_API_PASSPHRASE"),
    key("DUKASCOPY_MAINNET_API_SECRET"),
    key("DUKASCOPY_SIM_API_KEY"),
    key("DUKASCOPY_SIM_API_PASSPHRASE"),
    key("DUKASCOPY_SIM_API_SECRET"),
    key("FXCM_DEMO_API_KEY"),
    key("FXCM_DEMO_API_PASSPHRASE"),
    key("FXCM_DEMO_API_SECRET"),
    key("FXCM_LIVE_API_KEY"),
    key("FXCM_LIVE_API_PASSPHRASE"),
    key("FXCM_LIVE_API_SECRET"),
    key("FXCM_MAINNET_API_KEY"),
    key("FXCM_MAINNET_API_PASSPHRASE"),
    key("FXCM_MAINNET_API_SECRET"),
    key("FXCM_SIM_API_KEY"),
    key("FXCM_SIM_API_PASSPHRASE"),
    key("FXCM_SIM_API_SECRET"),
    key("HYPERLIQUID_BROKER_CODE"),
    key("HYPERLIQUID_BUILDER_CODE"),
    key("HYPERLIQUID_DEMO_API_KEY"),
    key("HYPERLIQUID_DEMO_API_PASSPHRASE"),
    key("HYPERLIQUID_DEMO_API_SECRET"),
    key("HYPERLIQUID_LIVE_API_KEY"),
    key("HYPERLIQUID_LIVE_API_PASSPHRASE"),
    key("HYPERLIQUID_LIVE_API_SECRET"),
    key("HYPERLIQUID_MAINNET_API_KEY"),
    key("HYPERLIQUID_MAINNET_API_PASSPHRASE"),
    key("HYPERLIQUID_MAINNET_API_SECRET"),
    key("HYPERLIQUID_SIM_API_KEY"),
    key("HYPERLIQUID_SIM_API_PASSPHRASE"),
    key("HYPERLIQUID_SIM_API_SECRET"),
    key("IBKR_DEMO_API_KEY"),
    key("IBKR_DEMO_API_PASSPHRASE"),
    key("IBKR_DEMO_API_SECRET"),
    key("IBKR_LIVE_API_KEY"),
    key("IBKR_LIVE_API_PASSPHRASE"),
    key("IBKR_LIVE_API_SECRET"),
    key("IBKR_MAINNET_API_KEY"),
    key("IBKR_MAINNET_API_PASSPHRASE"),
    key("IBKR_MAINNET_API_SECRET"),
    key("IBKR_SIM_API_KEY"),
    key("IBKR_SIM_API_PASSPHRASE"),
    key("IBKR_SIM_API_SECRET"),
    key("IG_DEMO_API_KEY"),
    key("IG_DEMO_API_PASSPHRASE"),
    key("IG_DEMO_API_SECRET"),
    key("IG_LIVE_API_KEY"),
    key("IG_LIVE_API_PASSPHRASE"),
    key("IG_LIVE_API_SECRET"),
    key("IG_MAINNET_API_KEY"),
    key("IG_MAINNET_API_PASSPHRASE"),
    key("IG_MAINNET_API_SECRET"),
    key("IG_SIM_API_KEY"),
    key("IG_SIM_API_PASSPHRASE"),
    key("IG_SIM_API_SECRET"),
    key("OANDA_DEMO_API_KEY"),
    key("OANDA_DEMO_API_PASSPHRASE"),
    key("OANDA_DEMO_API_SECRET"),
    key("OANDA_LIVE_API_KEY"),
    key("OANDA_LIVE_API_PASSPHRASE"),
    key("OANDA_LIVE_API_SECRET"),
    key("OANDA_MAINNET_API_KEY"),
    key("OANDA_MAINNET_API_PASSPHRASE"),
    key("OANDA_MAINNET_API_SECRET"),
    key("OANDA_SIM_API_KEY"),
    key("OANDA_SIM_API_PASSPHRASE"),
    key("OANDA_SIM_API_SECRET"),
    key("OKX_BROKER_CODE"),
    key("OKX_BUILDER_CODE"),
    key("OKX_DEMO_API_KEY"),
    key("OKX_DEMO_API_PASSPHRASE"),
    key("OKX_DEMO_API_SECRET"),
    key("OKX_LIVE_API_KEY"),
    key("OKX_LIVE_API_PASSPHRASE"),
    key("OKX_LIVE_API_SECRET"),
    key("OKX_MAINNET_API_KEY"),
    key("OKX_MAINNET_API_PASSPHRASE"),
    key("OKX_MAINNET_API_SECRET"),
    key("OKX_SIM_API_KEY"),
    key("OKX_SIM_API_PASSPHRASE"),
    key("OKX_SIM_API_SECRET"),
    key("POLYMARKET_BROKER_CODE"),
    key("POLYMARKET_BUILDER_CODE"),
    key("POLYMARKET_DEMO_API_KEY"),
    key("POLYMARKET_DEMO_API_PASSPHRASE"),
    key("POLYMARKET_DEMO_API_SECRET"),
    key("POLYMARKET_LIVE_API_KEY"),
    key("POLYMARKET_LIVE_API_PASSPHRASE"),
    key("POLYMARKET_LIVE_API_SECRET"),
    key("POLYMARKET_MAINNET_API_KEY"),
    key("POLYMARKET_MAINNET_API_PASSPHRASE"),
    key("POLYMARKET_MAINNET_API_SECRET"),
    key("POLYMARKET_SIM_API_KEY"),
    key("POLYMARKET_SIM_API_PASSPHRASE"),
    key("POLYMARKET_SIM_API_SECRET"),
    // vike:new-venue:row // TODO(new-venue: {venue}): the twelve rows below are this venue's
    // vike:new-venue:row // WHOLE generated grid (`vike_model::credential_keys`' four tiers x
    // vike:new-venue:row // three suffixes). They need no decision — but they were APPENDED,
    // vike:new-venue:row // not merged: move them to this block's alphabetical position, then
    // vike:new-venue:row // delete this comment. `{VENUE}_BROKER_CODE`/`{VENUE}_BUILDER_CODE`
    // vike:new-venue:row // join them if this venue's `vike_model::attribution` arm ever stops
    // vike:new-venue:row // being `AttributionMechanic::None` — until then nothing looks them up.
    // vike:new-venue:row key("{VENUE}_DEMO_API_KEY"),
    // vike:new-venue:row key("{VENUE}_DEMO_API_PASSPHRASE"),
    // vike:new-venue:row key("{VENUE}_DEMO_API_SECRET"),
    // vike:new-venue:row key("{VENUE}_LIVE_API_KEY"),
    // vike:new-venue:row key("{VENUE}_LIVE_API_PASSPHRASE"),
    // vike:new-venue:row key("{VENUE}_LIVE_API_SECRET"),
    // vike:new-venue:row key("{VENUE}_MAINNET_API_KEY"),
    // vike:new-venue:row key("{VENUE}_MAINNET_API_PASSPHRASE"),
    // vike:new-venue:row key("{VENUE}_MAINNET_API_SECRET"),
    // vike:new-venue:row key("{VENUE}_SIM_API_KEY"),
    // vike:new-venue:row key("{VENUE}_SIM_API_PASSPHRASE"),
    // vike:new-venue:row key("{VENUE}_SIM_API_SECRET"),
];
