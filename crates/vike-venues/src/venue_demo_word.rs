//! **What a venue itself calls our `demo` tier** — a DISPLAY hint, never parsed (decision 0156,
//! item 8).
//!
//! An account's tier is one of three words, `paper`, `demo` and `live` (`vike_model::AccountTier`), and the
//! operator writes it once, in `account.tier`. A venue has its own name for the environment our
//! `demo` reaches — IBKR's "paper account", Hyperliquid's "testnet", OANDA's "practice" — and that
//! name stays inside the bridge, where it picks a host, a network or a gateway by matching the tier.
//! This table is the one place the venue's word reaches the operator: beside the tier where it is
//! picked (`vike-cli secrets account add|set-tier`) and where it is read (`vike-cli secrets
//! accounts`), so `demo (ibkr: paper account)` says which IBKR environment a `demo` row trades on.
//!
//! ⚠ **Never parsed.** No reader turns a venue's word back into a tier, and a bridge never asks
//! this table which host to dial: a second place a tier is spelled is a map to keep in two
//! directions (decision 0156's Why). The word is what the bridge's own `demo` endpoint is CALLED by
//! the venue, read off that endpoint, so a row cites the bridge code it was read from.
//!
//! The table follows `crates/vike-venues/CLAUDE.md`'s "Per-venue capability maps (the playbook)":
//! one NAMED row per roster venue, a verbatim matrix pin, a completeness test over
//! [`vike_model::VENUES`]. `None` is a venue with no environment our `demo` could reach (polymarket) or a
//! string the roster does not carry.

/// **What `venue` itself calls the environment our `demo` tier trades on**, or `None` for a venue
/// with no such environment or a string the roster does not carry. Display only (the module doc).
#[must_use]
pub fn venue_demo_word(venue: &str) -> Option<&'static str> {
    named_row(venue).flatten()
}

/// The NAMED rows; the outer `None` is a venue this table has never heard of. Separate from
/// [`venue_demo_word`] because a named "no demo" row and the fallback answer the same `None`, and
/// only this split lets the completeness test tell "classified" from "forgotten".
fn named_row(venue: &str) -> Option<Option<&'static str>> {
    Some(match venue {
        // `crates/bridges/binance/src/spot.rs`'s `DEMO_REST` (`demo-api.binance.com`) and
        // `crates/bridges/binance/src/perp.rs`'s `DEMO_FAPI_REST` (`demo-fapi.binance.com`): Binance
        // Demo Trading, not the older spot/futures testnets.
        "binance" => Some("demo trading"),
        // `crates/bridges/bybit/src/perp.rs`'s `DEMO_REST` (`api-demo.bybit.com`): Bybit Demo Trading,
        // not `api-testnet.bybit.com`.
        "bybit" => Some("demo trading"),
        // `crates/bridges/okx/src/account_config.rs`: the `x-simulated-trading` header on the
        // production hosts — what OKX calls Demo Trading.
        "okx" => Some("demo trading"),
        // `crates/bridges/deribit/src/transport.rs`'s `TESTNET_REST` (`test.deribit.com`).
        "deribit" => Some("testnet"),
        // `crates/bridges/oanda/src/config.rs`: the `fxpractice` hosts, OANDA's practice accounts.
        "oanda" => Some("practice"),
        // `crates/bridges/ig/src/config.rs`: `demo-api.ig.com`.
        "ig" => Some("demo"),
        // `crates/bridges/fxcm/src/config.rs`: the ForexConnect `Demo` connection.
        "fxcm" => Some("demo"),
        // `crates/bridges/dukascopy/CLAUDE.md`: the DEMO1 / DEMO2 demo accounts.
        "dukascopy" => Some("demo"),
        // No test environment: every Polymarket order is a mainnet order on Polygon.
        "polymarket" => None,
        // `crates/bridges/vike-ibkr/CLAUDE.md`: the IBKR paper trading account (IBC's
        // `--mode=paper`). ⚠ NOT our `paper`, which is the local simulator and reaches no venue.
        "ibkr" => Some("paper account"),
        // `crates/bridges/ctrader/src/config.rs`'s `ctrader_host` (`demo.ctraderapi.com`).
        "ctrader" => Some("demo"),
        // `crates/bridges/alpaca/src/hosts.rs`'s `hosts_for` (`broker-api.sandbox.alpaca.markets`):
        // the Broker API SANDBOX, not the Trading API's `paper-api` host, which the bridge never dials.
        "alpaca" => Some("sandbox"),
        // `crates/bridges/aster/src/urls.rs` (`fapi.asterdex-testnet.com`).
        "aster" => Some("testnet"),
        // `crates/bridges/hyperliquid/src/base/consts.rs`'s `TESTNET_INFO`
        // (`api.hyperliquid-testnet.xyz`).
        "hyperliquid" => Some("testnet"),
        // vike:new-venue:row // TODO(new-venue: {venue}): what does the venue CALL the environment the bridge's demo
        // vike:new-venue:row // endpoint reaches ("testnet", "demo", "practice")? Cite the endpoint; `None` only for a venue with none.
        // vike:new-venue:row "{venue}" => None,
        _ => return None,
    })
}

#[path = "venue_demo_word_tests.rs"]
#[cfg(test)]
mod venue_demo_word_tests;
