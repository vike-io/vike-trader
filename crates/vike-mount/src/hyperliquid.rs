//! **The one residual decision 0088's B3 deliberately left here** — everything else this file used
//! to hold (the signer/transport wiring, the `userRole` identity probe, the fallback grid, the
//! live exec+recon client builder) moved down into `crates/bridges/hyperliquid/src/mount.rs`
//! (`docs/decisions/0088-mount-sheds-venue-facts-to-their-bridges.md`'s B3). That whole file had
//! been sitting in `vike-mount` only because it was extracted VERBATIM from `vike-app/src/main.rs`
//! when the GUI's local composition root was cut — nobody had decided it belonged here.
//!
//! What is left here is the pure INTENT probe [`would_mount_live`] — the ceiling chooses the
//! network (`vike_hyperliquid::config::Env::for_ceiling`, decision 0095). `hl_env`, the
//! `HYPERLIQUID_MAINNET` process-environment read decision 0088 left here as residual B2, was
//! deleted by decision 0095: the ceiling now decides the network directly, and no variable is read
//! anywhere in this file. [`would_mount_live`] resolves the SAME ceiling the mount arm resolves,
//! through the SAME bridge function (`vike_hyperliquid::config::Env::for_ceiling`), so the two can
//! never disagree about which tier's key gates the mount. It has no signer, no transport and no
//! network of its own; it is a thin key-presence check over
//! `vike_hyperliquid::config::load_for_account`.

use std::collections::HashMap;

use vike_model::account_keys::AccountLabel;

/// The PURE half of the live gate — key present for the ceiling-selected env — with NO network, no
/// signing, no side effects. Consumed by `crate::would_mount_live` (the pre-connect risk-budget
/// refusal). INTENT-based on purpose: a present-but-invalid key still probes live (the full mount
/// would decline later and fall to paper), because a key in the map IS the operator's declared
/// intent to trade this venue live.
///
/// `label` scopes it to ONE account: `HYPERLIQUID_{TIER}_PRIVATE_KEY__{LABEL}`, with no fallback
/// to the unlabelled key — so a labelled account with no key of its own probes FALSE rather than
/// reporting the default account's intent as its own.
pub(crate) fn would_mount_live(
    vars: &HashMap<String, String>,
    label: &AccountLabel,
    live_permitted: bool,
) -> bool {
    vike_hyperliquid::config::load_for_account(
        vike_hyperliquid::config::Env::for_ceiling(live_permitted),
        label,
        vars,
    )
    .is_some()
}

#[cfg(test)]
mod tests {
    /// **THE WIRING GATE, MOUNT'S HALF: `margin_mode_grid` → `Account` → `apply_fill`.**
    ///
    /// The DERIVATION half (venue `meta` → `vike_hyperliquid::mount::margin_mode`) is gated in the
    /// bridge now — `crates/bridges/hyperliquid/src/mount_tests.rs`'s
    /// `an_isolated_only_asset_resolves_isolated_and_an_ordinary_one_stays_cross` — because
    /// `margin_mode_grid` is `vike-mount`-only and the layer rule runs the other way (a bridge may
    /// not depend on `vike-mount`). This half stays here and drives the REAL bridge function across
    /// the crate boundary, exactly as `crate::make_engine`'s `("hyperliquid", _)` arm does, so the
    /// two halves are proven CONNECTED rather than each proven alone: a real `meta` body → the real
    /// `Symbology` → `vike_hyperliquid::mount::margin_mode` → `crate::margin_mode_grid` →
    /// `Account` → `apply_fill`.
    #[test]
    fn an_isolated_only_asset_folds_isolated_through_the_real_mount_chain() {
        use vike_exec::{Account, BalanceMode};
        use vike_hyperliquid::symbology::Symbology;
        use vike_model::MarginMode;

        // Real 2026-08-05 row shapes: CASHCAT is the one isolated-only asset still live.
        let meta = serde_json::json!({"universe": [
            {"name": "BTC", "szDecimals": 5, "maxLeverage": 40},
            {"name": "CASHCAT", "szDecimals": 0, "maxLeverage": 3, "onlyIsolated": true},
        ]});
        let symbology = Symbology::from_meta(&meta, &serde_json::json!({}));

        let fill = |symbol: &str| vike_model::events::FillEvent {
            trade_id: "t1".into(),
            client_order_id: "c1".to_string(),
            venue: vike_hyperliquid::consts::VENUE.into(),
            symbol: symbol.into(),
            side: 1,
            last_qty: 1.0,
            last_px: 1.0,
            commission: 0.0,
            commission_asset: String::new().into(),
            liquidity_side: "taker".into(),
            ts: 1,
            mark_price: None,
            position_side: "BOTH".into(),
        };
        // Exactly what `make_engine` does with the out-param the bridge writes.
        let booked = |symbol: &str| {
            let mode = vike_hyperliquid::mount::margin_mode(&symbology, symbol);
            let mut account =
                Account::new(1.0, vike_hyperliquid::consts::VENUE, None, BalanceMode::Delta)
                    .with_default_margin_modes(crate::margin_mode_grid(symbol, mode));
            account.apply_fill(&fill(symbol));
            let key: vike_exec::PositionKey =
                (vike_hyperliquid::consts::VENUE.into(), symbol.into(), "BOTH".into());
            account.positions[&key].margin_mode
        };

        assert_eq!(
            booked("CASHCAT"),
            MarginMode::Isolated,
            "isolated-only asset: the venue's per-ASSET truth must reach the fold"
        );
        assert_eq!(
            booked("BTC"),
            MarginMode::Cross,
            "ordinary asset in the SAME universe: unchanged, the per-venue default"
        );
        assert_eq!(
            booked("NOT-LISTED"),
            vike_model::caps_for(vike_hyperliquid::consts::VENUE).default_margin_mode,
            "an unlisted symbol falls back to the per-venue declaration, not a literal"
        );
    }
}
