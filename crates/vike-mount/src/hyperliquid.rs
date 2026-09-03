//! Hyperliquid's bespoke-signer live exec + reconcile client builder, extracted verbatim from
//! `vike-app/src/main.rs`. HL's `HYPERLIQUID_DEMO_*` credential shape (a secp256k1 private key +
//! optional master address) is NOT the standard `API_KEY/SECRET`, so it is loaded here rather than
//! via the generic `load_credentials_from` factory `crate::make_engine` uses for the CEX venues.
//! [`crate::make_engine`]'s `("hyperliquid", _)` arm is the sole caller. The fallback grid used only
//! on a meta-fetch failure lives here too (its only caller is [`hyperliquid_live_client`]).

use std::collections::HashMap;

use vike_model::account_keys::AccountLabel;

/// FALLBACK Hyperliquid BTC-perp grid (szDecimals-5 → tick 0.1 / step 1e-5, $10 min notional),
/// used ONLY if the adapter's startup `meta` fetch fails. The live client fetches the real grid.
fn hyperliquid_fallback_properties() -> vike_model::SymbolProperties {
    vike_model::SymbolProperties {
        tick_size: 0.1,
        step_size: 0.00001,
        min_qty: 0.00001,
        max_qty: 1_000_000.0,
        min_notional: 10.0,
        // Everything else absent: HL sizes in the base asset (no contract multiplier, = 1.0), the
        // fallback never invents tiers (the scalar tick IS the grid), and HL declares no venue
        // hold. Functional-update syntax so a future `SymbolProperties` field costs this site
        // nothing — see `fallback.rs` for the same reasoning.
        ..Default::default()
    }
}

/// Which HL network a mount targets: testnet by default (safe); opt into mainnet with the EXACT
/// string `HYPERLIQUID_MAINNET=1`, from the process env OR the workspace `.env` map (a set process
/// value wins) — the ONE converged workspace rule every switched venue now shares
/// (`vike_bridge_core::mainnet`, whose fold this fn consumes; the env/map reads themselves stay
/// HERE, resolvable by the settings-registry gate — see that module's env-boundary note).
///
/// ⚠ STEP 2 CHANGED this venue in the OTHER direction from the CEX ones: HL used to ALSO accept
/// case-insensitive `"true"`, and no longer does. An operator running with
/// `HYPERLIQUID_MAINNET=true` therefore DROPS TO TESTNET after this change and must respell the
/// flag as `1`. It fails safe (never a surprise mainnet), but it is a real behavior change for a
/// live deployment.
///
/// Extracted from [`hyperliquid_live_client`] so the PURE pre-connect probe below resolves the SAME
/// env from the SAME flag — the two can never disagree on which tier's key gates the mount — and
/// shared with vike-tradehub's feed-network plan through the same converged fold.
///
/// ⚠ `mainnet_permitted` is the deployment's ARMING CEILING for this venue, folded in as a
/// CONJUNCT — `true` only when `crates/vike-config/src/venue_mode.rs`'s `VenueMode::Live` is the
/// ceiling. It is the same shape as `crate::cex_mainnet_enabled`'s conjunct at the CEX venues and
/// exists for the same reason: an armed `HYPERLIQUID_MAINNET=1` under a `demo` ceiling must select
/// TESTNET, not mainnet, or the flag would route around the one file that says which venues this
/// box means to trade. It can only ever narrow — `false` forces Demo, `true` leaves the flag's own
/// answer untouched — so every caller that legitimately has no ceiling to consult passes `true` and
/// is byte-identical to before this parameter existed.
pub(crate) fn hl_env(
    vars: &HashMap<String, String>,
    mainnet_permitted: bool,
) -> vike_hyperliquid::config::Env {
    let process = std::env::var("HYPERLIQUID_MAINNET").ok();
    let mainnet = mainnet_permitted
        && vike_bridge_core::mainnet::mainnet_for(
            "hyperliquid",
            process.as_deref(),
            vars.get("HYPERLIQUID_MAINNET").map(String::as_str),
        );
    if mainnet {
        vike_hyperliquid::config::Env::Live
    } else {
        vike_hyperliquid::config::Env::Demo
    }
}

/// The mounted symbol's RULING margin mode, read off the venue's own `meta` — the mode a position
/// on this asset OPENS IN when the order names none.
///
/// Extracted as its own pure function for the same reason [`hl_env`] was: it is the fact
/// [`hyperliquid_live_client`] writes into its `margin_mode` out-param, and everything after it
/// (the grid, the `Account`, the fold) can then be gated end-to-end with a fixture `meta` and no
/// network — the live client itself needs credentials and a `/info` round trip before it ever
/// reaches a `Symbology`.
///
/// It is a THIN wrapper on purpose: the derivation belongs to the bridge
/// ([`vike_hyperliquid::symbology::InstrumentRef::effective_margin_mode`], #1087 — `only_isolated`
/// narrowing [`vike_model::VenueCaps::default_margin_mode`]), and duplicating the rule here would
/// be the second source of truth that PR deliberately avoided creating.
///
/// A symbol the venue does not list falls back to the per-VENUE declaration, not to a literal: an
/// unlisted symbol has no per-asset narrowing to apply, which is exactly what that field means.
pub(crate) fn hl_margin_mode(
    symbology: &vike_hyperliquid::symbology::Symbology,
    symbol: &str,
) -> vike_model::MarginMode {
    symbology.by_symbol(symbol).map(|i| i.effective_margin_mode()).unwrap_or_else(|| {
        vike_model::caps_for(vike_hyperliquid::consts::VENUE).default_margin_mode
    })
}

/// The ONE per-IP REST weight window a Hyperliquid mount opens, and a transport already riding it.
///
/// Hyperliquid meters `/info` reads and signed `/exchange` actions out of a SINGLE per-IP budget
/// (`vike_model::venue_rate_limits::HYPERLIQUID`'s `rest_ip_weight`), so the number of `RateGate`s
/// a mount builds IS the multiple of that budget the process is willing to spend — and that row
/// admits exactly what the venue publishes, holding nothing back to absorb a second one. Every
/// `vike_hyperliquid::transport::HyperliquidTransport::new` mints a FRESH gate, which is the whole
/// reason this function exists: it resolves the gate once
/// (`vike_hyperliquid::ratelimit::ip_weight_gate`) and hands back both halves
/// [`hyperliquid_live_client`] needs — the handle to clone into the exec spawn (which clones it on
/// into the funding poller), and a transport wired to that same window for the instruments load and
/// the `HyperliquidReconClient` that inherits it. Three REST consumers, one budget.
///
/// Split out of [`hyperliquid_live_client`] so the sharing is provable with no key, no signer and
/// no network: `an_hl_mounts_rest_paths_ride_one_ip_weight_window` takes the budget through one
/// half and watches it be gone from the other.
fn hl_ip_gate_and_transport(
    network: vike_hyperliquid::config::Network,
) -> (vike_bridge_core::ratelimit::RateGate, vike_hyperliquid::transport::HyperliquidTransport) {
    use vike_hyperliquid::transport::HyperliquidTransport;

    let ip_gate = vike_hyperliquid::ratelimit::ip_weight_gate();
    let transport = HyperliquidTransport::new(network).with_rate_gate(ip_gate.clone());
    (ip_gate, transport)
}

/// The PURE half of [`hyperliquid_live_client`]'s live gate — key present for the flag-selected
/// env — with NO network, no signing, no side effects. Consumed by `crate::would_mount_live`
/// (the pre-connect risk-budget refusal). INTENT-based on purpose: a present-but-invalid key
/// still probes live (the full client would decline later and fall to paper), because a key in
/// the map IS the operator's declared intent to trade this venue live.
///
/// `label` scopes it to ONE account: `HYPERLIQUID_{TIER}_PRIVATE_KEY__{LABEL}`, with no fallback
/// to the unlabelled key — so a labelled account with no key of its own probes FALSE rather than
/// reporting the default account's intent as its own.
pub(crate) fn would_mount_live(
    vars: &HashMap<String, String>,
    label: &AccountLabel,
    mainnet_permitted: bool,
) -> bool {
    vike_hyperliquid::config::load_for_account(hl_env(vars, mainnet_permitted), label, vars)
        .is_some()
}

/// Build Hyperliquid's live TESTNET exec client (+ its reconcile handle) from the bespoke
/// `HYPERLIQUID_DEMO_*` key shape (a secp256k1 private key + an optional master address — NOT the
/// standard API_KEY/SECRET, so it is loaded here rather than via `load_credentials_from`). `None`
/// when the key is absent (the live gate) or any startup step fails (bad key / meta fetch) → the
/// caller falls back to paper. On success sets `limits` from the fetched grid and returns
/// `(exec_client, recon_client)`. DEMO ⇒ testnet, matching the app's demo-orders convention for
/// every other crypto venue (market DATA stays mainnet public via `market_feed`).
///
/// The `HyperliquidReconClient` is built HERE rather than in `build_recon_client` because HL's
/// keyless-`/info`-against-master report reads need the bespoke `HyperliquidTransport`/master this
/// function already builds, not the standard `Credentials` factory `build_recon_client` takes. It
/// reads on the SAME network the exec trades (testnet by default) so recon sees the account the
/// orders actually land in; its `Product` (perp vs spot balance endpoint) is the mounted symbol's.
///
/// `market_slippage` is the DEPLOYMENT's requested emulated-market aggression band
/// (`market_slippage` in `<vike home>/policy.toml`, projected onto
/// [`crate::MountPolicy`] by the binary and threaded through `make_engine` — settings-unification
/// Phase 6c). Hyperliquid has no native market order: a market intent is priced as an `Ioc` limit
/// at `mid * (1 ± band)` and a tripped stop-MARKET at `trigger * (1 ± band)`, so this band is the
/// WORST price such an order is allowed to reach. It is resolved ONCE here, at the mount, through
/// `vike_hyperliquid::exec::market_slippage_for` — which returns the adapter's own historical
/// literal on `None` (byte-identical to every mount before this parameter existed) and CLAMPS an
/// out-of-range value into the workspace bounds, so no configuration can price market orders more
/// aggressively than the code already did. The resolved `f64` is then carried into the exec thread
/// rather than re-read there, the same "resolve at the mount" idiom the leverage decision, the
/// `{VENUE}_MAINNET` flag and the attribution code already follow.
///
/// `margin_mode` is the SECOND out-parameter, written the same way and for the same reason as
/// `limits`: it is a fact only the venue's `meta` fetch knows, and the `Account` that needs it is
/// built by the caller after this function returns. It receives the mounted symbol's RULING margin
/// mode — [`vike_hyperliquid::symbology::InstrumentRef::effective_margin_mode`], #1087's derivation
/// of `only_isolated` over the per-venue [`vike_model::VenueCaps::default_margin_mode`] — so a
/// position opened from flat on one of this venue's isolated-only assets books `Isolated` at the
/// FOLD rather than `Cross`. It is left untouched (the caller's `Cross`) on every path that does
/// not reach a live `meta`: an absent key, a bad key, or a failed fetch all drop to paper, where
/// there is no per-asset truth to read and the paper exchange models no isolated wallet anyway.
///
/// `mainnet_permitted` is this deployment's ARMING CEILING for hyperliquid, `true` only under
/// `venues.hyperliquid = "live"` — see [`hl_env`], which is where it folds in. It cannot widen
/// anything: `false` forces TESTNET regardless of `HYPERLIQUID_MAINNET`, `true` leaves the flag's
/// own answer alone.
///
/// `symbol_grids` is the THIRD out-parameter, on the same rationale as `limits` and `margin_mode`:
/// the `HyperliquidInstruments` load below resolves the WHOLE venue in one round trip, so this arm
/// can answer for `declared_legs` — the extra symbols the caller's mount declared — at no network
/// cost whatever. That is why hyperliquid is a `crate::DeclaredGridSource::InHand`
/// venues; see `crate::symbol_grid`'s module doc for why arms with a symbol-SCOPED fetch are
/// deliberately not wired. `declared_legs` empty (every mount in this workspace today) writes
/// nothing and reads nothing — the map is left exactly as the caller passed it.
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn hyperliquid_live_client(
    symbol: &str,
    declared_legs: &[String],
    vars: &HashMap<String, String>,
    label: &AccountLabel,
    live_events: &vike_exec::EventSender,
    limits: &mut vike_exec::RiskLimits,
    symbol_grids: &mut indexmap::IndexMap<String, vike_exec::SymbolGrid>,
    margin_mode: &mut vike_model::MarginMode,
    recon_trigger: Option<std::sync::mpsc::Sender<()>>,
    market_slippage: Option<f64>,
    mainnet_permitted: bool,
) -> Option<(Box<dyn vike_exec::ExecutionClient + Send>, Box<dyn vike_exec::recon::ReconClient>)> {
    use vike_hyperliquid::config;
    let env = hl_env(vars, mainnet_permitted);
    let mainnet = env == config::Env::Live;
    // ⚠ `load_for_account`, not `load` — this is the ONE line that keeps a labelled account from
    // signing with the DEFAULT account's private key. For `AccountLabel::Default` the two are the
    // same call over the same key names.
    let creds = config::load_for_account(env, label, vars)?; // absent key = the live gate → paper
    let signer =
        vike_hyperliquid::signing::Signer::from_private_key(&creds.private_key, creds.network)
            .map_err(|e| tracing::warn!("hyperliquid: invalid private key ({e}); staying paper"))
            .ok()?;
    let master = creds.account_address.clone().unwrap_or_else(|| signer.address().to_string());
    // ONE per-IP REST weight window for every REST path this mount opens — the instruments load and
    // the recon reads through `transport`, the exec thread's `/exchange` and the funding poller's
    // `/info` through `ip_gate`. See [`hl_ip_gate_and_transport`] for why a per-consumer gate is a
    // multiple of the venue's cap rather than a conservative default.
    let (ip_gate, transport) = hl_ip_gate_and_transport(creds.network);
    let instruments = match vike_hyperliquid::instruments::HyperliquidInstruments::load(&transport)
    {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!("hyperliquid: meta/spotMeta load failed ({e}); staying paper");
            return None;
        }
    };
    *limits =
        instruments.properties(symbol).map(vike_exec::RiskLimits::from_properties).unwrap_or_else(
            || vike_exec::RiskLimits::from_properties(&hyperliquid_fallback_properties()),
        );
    // The DECLARED LEGS' own grids, out of the SAME already-loaded `instruments` — no second round
    // trip, which is the whole reason this arm is wired at all (see the fn doc's `symbol_grids`
    // paragraph). Deliberately NOT falling back to `hyperliquid_fallback_properties` the way the
    // mounted symbol does: that fallback is a venue-wide guess that happens to be right for the
    // mounted market, and stamping it onto a leg would declare a grid nobody resolved. An unknown
    // leg therefore gets no row and `crate::symbol_grid::warn_ungridded_legs` reports it.
    *symbol_grids = crate::symbol_grid::declared_symbol_grids(symbol, declared_legs, |leg| {
        instruments.properties(leg).copied()
    });
    // Reconcile handle: reuse the SAME transport (moved in — the instruments load above only
    // borrowed it) + master this function already built; keyless `/info` reads against the master on
    // `creds.network`. The balance endpoint (perp `clearinghouseState` vs spot
    // `spotClearinghouseState`) follows the mounted symbol's product; default perp.
    let product = instruments
        .symbology()
        .by_symbol(symbol)
        .map(|i| i.product)
        .unwrap_or(vike_hyperliquid::config::Product::Perp);
    // The OTHER mount-time per-asset fact, from the same fetched `Symbology`: the mode a position
    // on this asset opens in (see the `margin_mode` out-param's note above, and [`hl_margin_mode`]
    // for the derivation). Written HERE and nowhere else — a paper fallback never reaches this
    // line, so it keeps the caller's `Cross`.
    *margin_mode = hl_margin_mode(instruments.symbology(), symbol);
    let recon: Box<dyn vike_exec::recon::ReconClient> =
        vike_hyperliquid::recon_client(transport, master.clone(), product);
    tracing::warn!(
        "hyperliquid: {} credentials present → LIVE exec client (real {} orders)",
        if mainnet { "LIVE" } else { "DEMO" },
        if mainnet { "MAINNET" } else { "testnet" }
    );
    // Builder Codes (task 7, unified-venue-attribution): resolved ONCE from the workspace `.env`.
    // `attribution_code_from` validates the address against `hyperliquid`'s
    // `AttributionMechanic::SignedBuilder`; absent/invalid degrades to `None` (byte-identical — no
    // `builder` object on the wire at all, matching every mount before this field existed). The
    // fee defaults to 0 (attribution-only, no on-chain `approveBuilderFee` needed — see
    // `vike_hyperliquid::builder_fee`'s module doc) unless `HYPERLIQUID_BUILDER_FEE_TENTHS_BP` is
    // set AND the operator has separately run the one-time approval from the master wallet.
    let builder =
        vike_bridge_core::credentials::attribution_code_from(vars, "hyperliquid").map(|address| {
            let fee_tenths_bp = vars
                .get("HYPERLIQUID_BUILDER_FEE_TENTHS_BP")
                .and_then(|s| s.trim().parse::<u32>().ok())
                .unwrap_or(0);
            vike_hyperliquid::signing::action::HlBuilderFee {
                address: address.trim().to_ascii_lowercase(),
                fee_tenths_bp,
            }
        });
    // The emulated-market band (Phase 6c), resolved ONCE here and carried into the exec thread.
    // `market_slippage_for(None)` IS `DEFAULT_MARKET_SLIPPAGE`, so an unconfigured mount is
    // byte-identical to the plain `spawn` this call replaced. Logged only when the operator
    // actually expressed a value, so the default path's log output is unchanged too — and a
    // clamped value has already been warned about by `resolve_market_slippage` itself.
    let applied_slippage = vike_hyperliquid::exec::market_slippage_for(market_slippage);
    if market_slippage.is_some() {
        tracing::info!(
            requested = ?market_slippage,
            applied = applied_slippage,
            "hyperliquid: emulated-market slippage band set from policy"
        );
    }
    let client: Box<dyn vike_exec::ExecutionClient + Send> =
        Box::new(vike_hyperliquid::HyperliquidExecutionClient::spawn_with_market_slippage(
            signer,
            master,
            std::sync::Arc::new(instruments),
            live_events.clone(),
            recon_trigger,
            builder,
            applied_slippage,
            // The same window `transport` (now owned by `recon`) charges — cloned on inside the
            // spawn into the funding poller's own transport.
            ip_gate,
        ));
    Some((client, recon))
}

#[cfg(test)]
mod tests {
    /// [`super::hl_env`] consumes hyperliquid's row in the shared per-venue switch table, so the
    /// fold is gated by it (a venue with no row can never be armed). Removing the row would
    /// silently pin HL to testnet forever — this pin makes that a deliberate edit. Pinned on the
    /// TABLE, not by running `hl_env`, deliberately: a runtime assertion here would flip on a
    /// genuinely exported `HYPERLIQUID_MAINNET`, the same stray-flag hazard
    /// `only_cex_venues_have_a_mainnet_cred_switch` (lib.rs) documents; the value grammar is
    /// covered exhaustively and env-free in `vike_bridge_core::mainnet`'s own tests.
    #[test]
    fn hl_env_consumes_the_shared_switch_row() {
        assert_eq!(
            vike_bridge_core::mainnet::mainnet_switch_for("hyperliquid"),
            Some(vike_bridge_core::mainnet::MainnetSwitch)
        );
    }

    /// STEP 2's one regression-shaped row, pinned at the venue that regressed: the fuzzy `"true"`
    /// spelling no longer arms hyperliquid, while the exact `"1"` still does — from EITHER source.
    /// Asserted through the shared fold (env-free) rather than by mutating global env, for the
    /// reason above.
    #[test]
    fn hyperliquid_no_longer_accepts_the_fuzzy_true_spelling() {
        use vike_bridge_core::mainnet::mainnet_for;
        assert!(!mainnet_for("hyperliquid", Some("true"), None), "exported `true` ⇒ TESTNET now");
        assert!(!mainnet_for("hyperliquid", None, Some("true")), ".env `true` ⇒ TESTNET now");
        assert!(mainnet_for("hyperliquid", Some("1"), None), "exported `1` still arms mainnet");
        assert!(mainnet_for("hyperliquid", None, Some("1")), ".env `1` still arms mainnet");
    }

    /// **THE SHARING GATE: one per-IP REST weight window per MOUNT, not one per transport.**
    ///
    /// Hyperliquid meters every REST call it serves — keyless `/info` reads and signed `/exchange`
    /// actions alike — out of a single per-IP budget, and
    /// `vike_model::venue_rate_limits::HYPERLIQUID`'s `rest_ip_weight` admits exactly what the
    /// venue publishes, so there is no headroom for a second window to hide in. Meanwhile
    /// `vike_hyperliquid::transport::HyperliquidTransport::new` mints a FRESH
    /// `vike_bridge_core::ratelimit::RateGate` on every call. Those two facts together are the
    /// hazard this pins: a mount that lets each consumer build its own transport runs N mutually
    /// invisible full-budget windows, every one of them correctly reporting itself inside quota
    /// while the process emits N times the venue's cap — answered with a 429 and then an IP ban,
    /// from the box that also signs live orders. It was UNCONDITIONALLY two (the exec thread and
    /// the funding poller) and three under `VIKE_RECONCILE=1`.
    ///
    /// [`super::hl_ip_gate_and_transport`] is where the mount refuses that, and both of its halves
    /// are load-bearing: the handle goes to
    /// `vike_hyperliquid::exec::HyperliquidExecutionClient::spawn_with_market_slippage` (which
    /// clones it on into the funding poller), and the transport is borrowed by the instruments load
    /// and then owned by the `HyperliquidReconClient`. A `RateGate` is an `Arc` inside, so "same
    /// window" is observable exactly the way `vike_bridge_core::ratelimit`'s
    /// `clone_shares_the_same_window` observes it — spend through one handle, watch the spend show
    /// up on the other. Driven through the REAL constructor rather than a rebuild of it, so
    /// substituting a fresh `ip_weight_gate()` on either side turns this red.
    #[test]
    fn an_hl_mounts_rest_paths_ride_one_ip_weight_window() {
        use vike_hyperliquid::config::Network;
        use vike_hyperliquid::transport::HyperliquidTransport;

        // The budget is READ from the row that owns it, never restated here — this test cannot
        // drift from `vike_model::venue_rate_limits` and does not become a second authority for it.
        let budget = vike_model::venue_rate_limits::HYPERLIQUID.rest_ip_weight.admitted();

        // CONTROL — the defect, still reproducible on the raw constructor. Two transports built the
        // plain way share nothing, which is what makes the assertion below mean something rather
        // than merely pass.
        let a = HyperliquidTransport::new(Network::Testnet);
        let b = HyperliquidTransport::new(Network::Testnet);
        assert!(a.rate_gate().try_proceed_cost(budget), "a fresh window admits its whole budget");
        assert!(
            b.rate_gate().try_proceed(),
            "two plainly-constructed transports must be INDEPENDENT windows — if this ever fails, \
             the control is broken and the real assertion below proves nothing"
        );

        // THE ASSERTION — the mount's own seam, both halves, one window.
        let (exec_gate, transport) = super::hl_ip_gate_and_transport(Network::Testnet);
        assert!(exec_gate.try_proceed_cost(budget), "the mount's window admits its whole budget");
        assert!(
            !transport.rate_gate().try_proceed(),
            "the exec half spent the whole per-IP budget, yet the instruments/recon transport \
             still admitted a call: the mount is running two full-budget windows against a venue \
             cap it declares with zero margin, each one reporting itself inside quota"
        );
    }

    /// **THE WIRING GATE: venue `meta` → fold, every link, no network.**
    ///
    /// `vike-exec`'s `fold_margin_mode.rs` gates the fold's half with a hand-built grid, and
    /// vike-hyperliquid's `effective_margin_mode_is_per_asset_not_per_venue` gates the derivation's
    /// half. Neither notices if the two are never CONNECTED — which was the whole bug: the venue's
    /// `recon_client` has computed the right `MarginMode` since #1081/#1087 and it reached nothing.
    /// This drives the real chain end to end: a real `meta` body → the real `Symbology` →
    /// [`super::hl_margin_mode`] → `vike_mount::margin_mode_grid` → `Account` → `apply_fill`.
    ///
    /// Both directions, because a fix that just hard-coded Isolated on this venue would pass the
    /// first half alone: the isolated-only asset must open ISOLATED, and the ordinary one sitting in
    /// the SAME universe under the same `VenueCaps` row must still open CROSS.
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
        // Exactly what `make_engine` does with the out-param this function writes.
        let booked = |symbol: &str| {
            let mode = super::hl_margin_mode(&symbology, symbol);
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
