//! Per-venue FALLBACK `SymbolProperties` grids + the OKX fallback contract value.
//!
//! Extracted verbatim from `vike-app/src/main.rs` (the venue→`ExecutionClient` composition root).
//! Each grid is used ONLY when a venue adapter's startup instrument fetch fails; the adapter fetches
//! the REAL per-symbol grid, so these are inert once that lands. See [`crate::make_engine`]'s live
//! arms for the call sites. Hyperliquid's fallback grid lives next to its live-client helper in
//! [`crate::hyperliquid`] (its only caller), not here.

/// FALLBACK OKX BTC-USDT-SWAP contract value (0.01 BTC/contract), used ONLY if the OKX adapter's
/// startup `instruments` fetch fails — the adapter fetches the real `ct_val` per instId.
pub(crate) const OKX_FALLBACK_CTVAL: f64 = 0.01;

/// FALLBACK Bybit linear-perp grid (BTCUSDT-tuned), used ONLY if the Bybit adapter's startup
/// `instruments-info` fetch fails — the adapter fetches the real per-symbol properties.
pub(crate) fn bybit_fallback_properties() -> vike_model::SymbolProperties {
    vike_model::SymbolProperties {
        tick_size: 0.1,
        step_size: 0.001,
        min_qty: 0.001,
        max_qty: 100_000.0,
        min_notional: 5.0,
        // Every remaining field stays ABSENT, via functional-update syntax: no contract size
        // (= multiplier 1.0), no tiered grid (the scalar tick above IS the whole grid), no venue
        // hold. A fallback fires only when the venue fetch FAILED, and inventing an unverified
        // value there would silently change the degraded path's behavior — absent is today's.
        // `..Default::default()` rather than an exhaustive literal ON PURPOSE: `SymbolProperties`
        // grows (contract_size, tick_scheme, taker_hold_ms all cost every construction site in the
        // workspace an edit), and a fallback grid must never have to be touched for a field it
        // does not know about.
        ..Default::default()
    }
}

/// FALLBACK OKX BTC-USDT-SWAP grid (contracts), used ONLY if the OKX adapter's startup `instruments`
/// fetch fails. See [`bybit_fallback_properties`].
pub(crate) fn okx_fallback_properties() -> vike_model::SymbolProperties {
    vike_model::SymbolProperties {
        tick_size: 0.1,
        step_size: 0.01,
        min_qty: 0.01,
        max_qty: 100_000.0,
        min_notional: 1.0,
        // Every remaining field stays ABSENT, via functional-update syntax: no contract size
        // (= multiplier 1.0), no tiered grid (the scalar tick above IS the whole grid), no venue
        // hold. A fallback fires only when the venue fetch FAILED, and inventing an unverified
        // value there would silently change the degraded path's behavior — absent is today's.
        // `..Default::default()` rather than an exhaustive literal ON PURPOSE: `SymbolProperties`
        // grows (contract_size, tick_scheme, taker_hold_ms all cost every construction site in the
        // workspace an edit), and a fallback grid must never have to be touched for a field it
        // does not know about.
        ..Default::default()
    }
}

/// FALLBACK Binance BTCUSDT grid, used ONLY if the Binance adapter's startup `exchangeInfo` fetch
/// fails. See [`bybit_fallback_properties`]. Spot-ish BTC values (the `.P` perp grid is coarser but
/// this is inert once the real grid loads via `fetch_binance_properties`).
pub(crate) fn binance_fallback_properties() -> vike_model::SymbolProperties {
    vike_model::SymbolProperties {
        tick_size: 0.01,
        step_size: 0.00001,
        min_qty: 0.00001,
        max_qty: 100_000.0,
        min_notional: 5.0,
        // Every remaining field stays ABSENT, via functional-update syntax: no contract size
        // (= multiplier 1.0), no tiered grid (the scalar tick above IS the whole grid), no venue
        // hold. A fallback fires only when the venue fetch FAILED, and inventing an unverified
        // value there would silently change the degraded path's behavior — absent is today's.
        // `..Default::default()` rather than an exhaustive literal ON PURPOSE: `SymbolProperties`
        // grows (contract_size, tick_scheme, taker_hold_ms all cost every construction site in the
        // workspace an edit), and a fallback grid must never have to be touched for a field it
        // does not know about.
        ..Default::default()
    }
}

/// FALLBACK Aster BTCUSDT grid, used ONLY if the Aster adapter's startup `exchangeInfo` fetch
/// fails. Aster is a Binance USDⓈ-M/spot fork, so the grid mirrors [`binance_fallback_properties`]
/// (identical values); inert once the real grid loads via `fetch_aster_properties`.
pub(crate) fn aster_fallback_properties() -> vike_model::SymbolProperties {
    vike_model::SymbolProperties {
        tick_size: 0.01,
        step_size: 0.00001,
        min_qty: 0.00001,
        max_qty: 100_000.0,
        min_notional: 5.0,
        // Every remaining field stays ABSENT, via functional-update syntax: no contract size
        // (= multiplier 1.0), no tiered grid (the scalar tick above IS the whole grid), no venue
        // hold. A fallback fires only when the venue fetch FAILED, and inventing an unverified
        // value there would silently change the degraded path's behavior — absent is today's.
        // `..Default::default()` rather than an exhaustive literal ON PURPOSE: `SymbolProperties`
        // grows (contract_size, tick_scheme, taker_hold_ms all cost every construction site in the
        // workspace an edit), and a fallback grid must never have to be touched for a field it
        // does not know about.
        ..Default::default()
    }
}

/// FALLBACK Deribit BTC-PERPETUAL grid, used ONLY if the adapter's startup `public/get_instrument`
/// fetch fails. Amounts are USD contracts on the coin-margined perp (`$10` step, `$0.50` index tick);
/// inert once the real grid loads via `fetch_deribit_properties`. No per-instrument max/min-notional
/// (matches the Deribit option-chain parser convention — `0.0` is treated as absent by the RiskGate).
pub(crate) fn deribit_fallback_properties() -> vike_model::SymbolProperties {
    vike_model::SymbolProperties {
        tick_size: 0.5,
        step_size: 10.0,
        min_qty: 10.0,
        max_qty: 0.0,
        min_notional: 0.0,
        // Every remaining field stays ABSENT, via functional-update syntax: no contract size
        // (= multiplier 1.0), no tiered grid (the scalar tick above IS the whole grid), no venue
        // hold. A fallback fires only when the venue fetch FAILED, and inventing an unverified
        // value there would silently change the degraded path's behavior — absent is today's.
        // `..Default::default()` rather than an exhaustive literal ON PURPOSE: `SymbolProperties`
        // grows (contract_size, tick_scheme, taker_hold_ms all cost every construction site in the
        // workspace an edit), and a fallback grid must never have to be touched for a field it
        // does not know about.
        ..Default::default()
    }
}
