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
    if mainnet { vike_hyperliquid::config::Env::Live } else { vike_hyperliquid::config::Env::Demo }
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
    directory: &crate::AccountDirectory,
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
    // ONE per-IP REST weight window for every REST path this mount opens — the instruments load and
    // the recon reads through `transport`, the exec thread's `/exchange` and the funding poller's
    // `/info` through `ip_gate`. See [`hl_ip_gate_and_transport`] for why a per-consumer gate is a
    // multiple of the venue's cap rather than a conservative default.
    let (ip_gate, transport) = hl_ip_gate_and_transport(creds.network);
    // ⚠ **WHOSE BOOK THIS KEY TRADES, ASKED OF THE VENUE RATHER THAN ASSUMED.**
    //
    // This line used to be `creds.account_address.unwrap_or_else(|| signer.address())`, and the
    // fallback carried an unverified assumption: *no configured address means the key IS the
    // account*. That is true for a plain master key and FALSE for an agent wallet — and Hyperliquid
    // warns about exactly this, because an agent address through an account-shaped read answers
    // `accountValue: "0.0"` rather than an error. So the wrong branch produced a mount that reads an
    // EMPTY book and looks healthy: recon sees nothing to reconcile, every position reads flat.
    //
    // `userRole` is the one read that goes AGENT → MASTER, so the assumption becomes a question.
    // Asked ONCE here, at mount, and never again — see `vike_hyperliquid::user_role` for why (weight
    // 60 of a 1200/min per-IP budget, the most expensive `/info` request the venue has).
    //
    // ⚠ **The probe may never turn a working mount into a paper one.** Every arm below falls back to
    // today's behaviour and says why; a network failure, an unknown role or a lapsed approval all
    // warn and continue. A mount that refused because an IDENTITY read timed out would trade the
    // venue's availability for its own.
    let master = match creds.account_address.clone() {
        // Configured outright — and **AUDITED SINCE 2026-09-20, where it used to be trusted
        // blind.**
        //
        // This arm reached no probe at all, so the best-configured account was the one that never
        // confirmed: no `ConfirmationRecord`, and `account.last_verified_at` stuck at
        // `NEVER VERIFIED` forever on the very account whose operator did the thing every warning
        // in this file tells them to do. It also left the configured address — the value deciding
        // which account this process reads and trades — as the ONE input in this venue's path that
        // nothing anywhere checked.
        //
        // It now asks the venue and COMPARES, and it still mounts on the configured value whatever
        // comes back. [`audit_configured_hyperliquid_master`] carries the whole argument for why it
        // audits rather than re-routes, and what the extra request costs.
        Some(addr) => audit_configured_hyperliquid_master(
            &transport,
            signer.address(),
            label,
            if mainnet { vike_config::VenueMode::Live } else { vike_config::VenueMode::Demo },
            addr,
            directory,
        ),
        // No address written: the probe is the ONLY thing that can name the account, so its answer
        // is what this mount USES rather than merely what it checks. Same request, same venue
        // answer, opposite authority — that is the whole difference between the two arms.
        None => resolve_hyperliquid_master(
            &transport,
            signer.address(),
            label,
            if mainnet { vike_config::VenueMode::Live } else { vike_config::VenueMode::Demo },
            directory,
        ),
    };
    // ⚠ `load_from_vars`, not `load`: the HIP-3 universe opt-in is a SETTING now
    // (`flags.hyperliquid_hip3`), folded into this same `vars` map by the composition root. The
    // process-env read it always had is still first inside that call, so an exported
    // `HYPERLIQUID_HIP3=1` behaves exactly as before.
    let instruments = match vike_hyperliquid::instruments::HyperliquidInstruments::load_from_vars(
        &transport, None, vars,
    ) {
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

/// **Which account does this signing key trade for?** — asked of the venue, with every failure
/// falling back to the address the key derives.
///
/// The fallback is TODAY'S behaviour, so nothing this function does can make a mount worse than it
/// was; what it adds is knowing, and saying, when that fallback is a guess. Each arm names the cure,
/// which is the same one line in every case: write the account address beside the key.
///
/// ⚠ **Not `Result`.** A caller that had to handle an error here would have to decide whether an
/// identity probe failing should stop a mount, and the answer is always no — see the call site's own
/// note. Folding the decision in here keeps it from being re-made differently at a second call site.
fn resolve_hyperliquid_master(
    transport: &vike_hyperliquid::transport::HyperliquidTransport,
    signer_address: &str,
    label: &AccountLabel,
    bound_tier: vike_config::VenueMode,
    directory: &crate::AccountDirectory,
) -> String {
    let answer = vike_hyperliquid::user_role::user_role(transport, signer_address);
    let decided = hl_master_from_role(answer.as_ref().map_err(|e| e.to_string()), signer_address);
    if let Some(warning) = &decided.warning {
        tracing::warn!(account = %label, address = %signer_address, "hyperliquid: {warning} {}", hl_address_cure(label));
        // ⚠ Nothing is said about RECORDING a book here, deliberately. Every arm carrying a warning
        // is one where the venue did NOT answer — a network failure, a lapsed approval, a role this
        // build does not classify — and `decided.address` is then today's FALLBACK rather than an
        // observation. Telling an operator to write a guess into the column an order routes on is
        // the one way this feature could make a box worse than it was.
        return decided.address;
    }
    if decided.address != signer_address {
        tracing::info!(
            account = %label,
            master = %decided.address,
            "hyperliquid: this key is an AGENT wallet — the venue says it signs for the account \
             above, which is the book this mount will read and trade. {}",
            hl_address_cure(label)
        );
    }
    // **THE VENUE ANSWERED, SO THE STORE CAN BE TOLD.** Reached only where `warning` is `None`,
    // which is the pair of arms in which Hyperliquid itself classified the address: it named the
    // master an agent signs for, or it agreed the key IS the account. Both are the venue's own
    // statement about which book this mount trades — the third state
    // `crate::book_identity::BookIdentity` has no variant for.
    //
    // ⚠ It REPORTS and does not write, and that is a rule rather than a stage of work:
    // `crates/vike-ops/tests/settings_registry.rs`'s `CREDENTIAL_STORE_PIN` is a ratchet over a
    // LIBRARY opening the credential store at a path its caller cannot see — and `vike-mount` is
    // where that defect was caught before. The composition root is the one that may write; what a
    // mount owes the operator is the fact and the exact line that records it.
    //
    // ⚠ DECLARED BLIND SPOT, and the same one the shared-book `warn!` and `crate::dukascopy`'s
    // `record_confirmation` carry: this EMISSION is reachable by no test. Getting here needs a live
    // `userRole` round trip against a real Hyperliquid endpoint, so a test that reached this
    // statement would have dialled the venue to do it. What IS gated is everything it DECIDES —
    // `crate::book_identity::confirmation_for_account` (the address) and
    // `vike_model::account_confirmation::verdict` (the disposition) are both pure and both driven by
    // tests. Keep the logic OUT of this block for exactly that reason.
    record_hyperliquid_confirmation(label, bound_tier, &decided.address, directory);
    decided.address
}

/// **What the venue said about an address the OPERATOR wrote** — the verdict behind the audit of a
/// configured `HYPERLIQUID_{TIER}_ACCOUNT_ADDRESS`.
///
/// A separate type from `vike_model::account_confirmation::Verdict` on purpose, because it compares
/// a different PAIR. That one compares the venue's answer against the `account` TABLE; this one
/// compares it against the CREDENTIAL STORE's setting — the value that decides which address this
/// mount actually reads and trades. They can disagree independently and mean different things.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ConfiguredAudit {
    /// The venue named exactly the account the operator wrote. Nothing is wrong and the handshake
    /// is worth recording — this is what stamps `account.last_verified_at`.
    Confirms,
    /// ⚠ **The venue named a DIFFERENT account.** The key cannot act for the address this mount was
    /// told to use, so either the setting is stale/mistyped or the key was re-approved elsewhere.
    Disagrees {
        /// What the venue answered, normalised.
        venue_said: String,
    },
    /// The venue did not classify the address — a network failure, a lapsed approval, a role this
    /// build does not know. **Says nothing**, because there is nothing to say: an unanswered probe
    /// is not evidence against a configured value.
    Unanswered,
}

/// The audit, PURE — over the two strings, so every arm is driven by a test instead of by a live
/// mount.
///
/// ⚠ **Both sides are normalised** (`crate::book_identity::normalize_book`: trim + lowercase). An
/// EVM address is the same account in any case, the operator types it however their wallet printed
/// it, and Hyperliquid answers lowercase — so a raw comparison would raise
/// [`ConfiguredAudit::Disagrees`] on every single boot of a perfectly correct box. That is the one
/// failure mode that would make an operator learn to ignore this alarm.
fn hl_audit_configured(decided: &HlMaster, configured: &str) -> ConfiguredAudit {
    if decided.warning.is_some() {
        return ConfiguredAudit::Unanswered;
    }
    let said = crate::book_identity::normalize_book(&decided.address);
    if said == crate::book_identity::normalize_book(configured) {
        ConfiguredAudit::Confirms
    } else {
        ConfiguredAudit::Disagrees { venue_said: said }
    }
}

/// **Audit an address the operator WROTE, against the venue's own answer** — and return the address
/// this mount will use, which is ALWAYS the configured one.
///
/// # ⚠ It audits; it does not re-route
///
/// The returned value is `configured`, unchanged, on every arm including
/// [`ConfiguredAudit::Disagrees`]. That is deliberate and it is the same posture
/// `vike_model::account_confirmation::verdict` takes for its own disagreement:
///
/// * **it does not switch to the venue's answer.** That would move which account this process reads
///   and trades, silently, on a boot the operator did not ask for — a routing change nobody
///   requested is worse than a wrong value they can be told about;
/// * **it does not refuse the mount.** The session authenticated. Taking the venue away over a
///   bookkeeping disagreement degrades a capability that was working
///   (`docs/decisions/0013-degrade-vs-refuse.md`), and the credential schema's §8 rules this exact
///   case: *a violation discovered AT A HANDSHAKE is REPORTED, not thrown*;
/// * **it parks NOTHING on a disagreement.** The parked record would say *the venue calls this
///   account X*, and `vike-cli secrets confirm` would fold X into `account.venue_account_id` —
///   while this mount is reading and trading the configured address instead. The table and the
///   routing would then disagree with nothing saying so, which is a second wrong-row-that-reads-as-
///   fine built on top of the first. So a disagreement reports and writes nothing, anywhere.
///
/// # What it costs, and why that was a decision rather than an omission
///
/// One `userRole` request per hyperliquid mount that has a configured address — weight 60 of a
/// shared 1200-per-minute per-IP budget, once, at startup. Until 2026-09-20 this path made NO
/// request at all and the account was never verified; `vike_hyperliquid::user_role`'s module doc
/// carries what the weight means and why the probe may never sit in a loop.
///
/// What it buys is the one comparison nothing else in the tree performs. Every other check reads the
/// configured address and trusts it. If it is stale or mistyped, this mount reads the WRONG
/// account — and Hyperliquid answers a wrong address with `accountValue: "0.0"` rather than an
/// error, so the result is a book that looks flat and healthy while reconcile finds nothing to
/// reconcile. That is precisely the failure `vike_model::account_confirmation` exists to remove, on
/// the one input that had no check at all.
fn audit_configured_hyperliquid_master(
    transport: &vike_hyperliquid::transport::HyperliquidTransport,
    signer_address: &str,
    label: &AccountLabel,
    bound_tier: vike_config::VenueMode,
    configured: String,
    directory: &crate::AccountDirectory,
) -> String {
    let answer = vike_hyperliquid::user_role::user_role(transport, signer_address);
    let decided = hl_master_from_role(answer.as_ref().map_err(|e| e.to_string()), signer_address);
    match hl_audit_configured(&decided, &configured) {
        // ⚠ SILENT, and deliberately so. An unanswered probe is not evidence against a configured
        // value, and this arm is reachable by an ordinary network blip — a line here would teach an
        // operator that the family is noise. The warning `resolve_hyperliquid_master` emits on the
        // same outcome is for the opposite case, where the fallback is all there is.
        ConfiguredAudit::Unanswered => {}
        ConfiguredAudit::Confirms => {
            // Nothing is WRONG, so nothing is warned — but the handshake is still worth recording:
            // this is the only thing that ever moves this account off `NEVER VERIFIED`, and the
            // whole reason the column exists is that *never verified* and *verified today* must not
            // look alike.
            record_hyperliquid_confirmation(label, bound_tier, &configured, directory);
        }
        ConfiguredAudit::Disagrees { venue_said } => tracing::error!(
            account = %label,
            configured = %configured,
            venue_answered = %venue_said,
            "hyperliquid: ⚠ THE CONFIGURED ADDRESS AND THE VENUE DISAGREE. \
             `HYPERLIQUID_{{TIER}}_ACCOUNT_ADDRESS` for this account says `{configured}`, and the \
             venue's own `userRole` says this key signs for `{venue_said}`. This mount is using the \
             CONFIGURED address — nothing was re-routed and nothing was written, because switching \
             accounts on a boot you did not ask for is worse than telling you. ⚠ If the configured \
             address is wrong, every read for this account is answering about somebody else's book, \
             and Hyperliquid answers a wrong address with `accountValue: \"0.0\"` rather than an \
             error — so it looks FLAT and HEALTHY rather than broken. Resolve it once: either \
             correct the key to `{venue_said}`, or re-approve this API wallet under the account you \
             meant."
        ),
    }
    configured
}

/// **The venue answered, so the store can be told** — this venue's call into the shared fold.
///
/// ⚠ **The body moved to [`crate::book_identity::record_confirmation`] and this is now a one-line
/// delegation**, because the blind-CEX work gave binance, okx and deribit an answer to record and
/// the three things this did — address the store's row, compare, park — were never hyperliquid
/// facts. What stays here is the one thing that is: `userRole` is what this venue answered WITH,
/// and an operator reading a disagreement needs to be told which probe to go and re-run.
///
/// Reached only where `HlMaster::warning` is `None`, which is the pair of arms in which Hyperliquid
/// itself classified the address: it named the master an agent signs for, or it agreed the key IS
/// the account. Both are the venue's own statement about which book this mount trades — the third
/// state [`crate::book_identity::BookIdentity`] has no variant for.
fn record_hyperliquid_confirmation(
    label: &AccountLabel,
    bound_tier: vike_config::VenueMode,
    master: &str,
    directory: &crate::AccountDirectory,
) {
    crate::book_identity::record_confirmation(
        "hyperliquid",
        "`userRole`",
        label,
        bound_tier,
        master,
        directory,
    );
}

/// The key an operator would write to settle the question outright, spelled rather than described —
/// an operator acts on a paste-able key, not on a sentence about one.
fn hl_address_cure(label: &AccountLabel) -> String {
    match label.text() {
        None => "Set `HYPERLIQUID_{TIER}_ACCOUNT_ADDRESS` to state it outright.".to_string(),
        Some(l) => format!("Set `HYPERLIQUID_{{TIER}}_ACCOUNT_ADDRESS__{l}` to state it outright."),
    }
}

/// What the resolver decided: the address to mount on, and the warning that should accompany it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HlMaster {
    address: String,
    /// `None` when the answer was CONFIRMED by the venue — either it named the master or it agreed
    /// the key is the account. Every other outcome carries the sentence that says so.
    warning: Option<String>,
}

/// **Role → the address to mount on**, PURE.
///
/// Split from the I/O so the branching — which arm trusts the venue, which falls back, which warns —
/// is reachable from a test without a network, a key or a transport. That branching is the whole of
/// the risk here: every fallback returns TODAY'S behaviour, so a wrong arm is silent by construction.
fn hl_master_from_role(
    answer: Result<&vike_hyperliquid::user_role::UserRole, String>,
    signer_address: &str,
) -> HlMaster {
    use vike_hyperliquid::user_role::UserRole;
    let fallback =
        |warning: String| HlMaster { address: signer_address.to_string(), warning: Some(warning) };
    match answer {
        // The key signs for somebody else. This is the branch the whole probe exists for, and it is
        // the one the old fallback got WRONG — silently, into an empty book.
        Ok(UserRole::Agent { master }) => HlMaster { address: master.clone(), warning: None },
        // The venue CONFIRMS the key is the account. Same answer the old code assumed, now verified
        // rather than guessed — which is the whole difference between this arm and the ones below.
        Ok(UserRole::User) => HlMaster { address: signer_address.to_string(), warning: None },
        // ⚠ An agent whose approval EXPIRED answers `missing`, not `agent`. So this is rarely a typo
        // — it is most often a key that can no longer sign for anyone, and the orders it is about to
        // place will be rejected by the venue.
        Ok(UserRole::Missing) => fallback(
            "the venue does not know this address. For an API wallet the usual cause is an EXPIRED \
             approval — the key can no longer sign for anyone and orders will be rejected. \
             Mounting on the key's own address, which will read an EMPTY book."
                .to_string(),
        ),
        // A sub-account or vault answer for something that is supposed to be a signing key, or a
        // role this build does not classify. The named master is still better than the signer.
        Ok(other) => HlMaster {
            address: other.master().unwrap_or(signer_address).to_string(),
            warning: Some(format!(
                "the venue classified this address as {other:?}, which this build does not map to a \
                 signing key."
            )),
        },
        // ⚠ The probe failed to ASK. Never a reason to refuse the mount: that would trade the
        // venue's REST availability for this box's ability to start.
        Err(e) => fallback(format!(
            "could not ask the venue which account this key trades for ({e}), so the key's own \
             address is assumed to BE the account — true for a master key, WRONG for an API wallet, \
             where it reads an empty book."
        )),
    }
}

#[cfg(test)]
mod master_resolution_tests {
    use super::*;
    use vike_hyperliquid::user_role::UserRole;

    const SIGNER: &str = "0x1111111111111111111111111111111111111111";
    const MASTER: &str = "0x85ecf584f25db6f146718b86d493e33c5af72052";

    /// ⚠ **THE BRANCH THE PROBE EXISTS FOR.** An agent key mounts on the MASTER, not on itself, and
    /// says nothing alarming — the venue answered, so there is no guess left to warn about.
    #[test]
    fn an_agent_key_mounts_on_the_master_the_venue_named() {
        let role = UserRole::Agent { master: MASTER.into() };
        let d = hl_master_from_role(Ok(&role), SIGNER);
        assert_eq!(d.address, MASTER);
        assert_eq!(d.warning, None, "a confirmed answer is not a warning");
    }

    /// The venue CONFIRMS the key is the account. Byte-identical to the old behaviour — and the
    /// point is that it is now confirmed rather than assumed, which is why it carries no warning
    /// while the fallbacks below do.
    #[test]
    fn a_plain_key_mounts_on_itself_and_is_not_warned_about() {
        let d = hl_master_from_role(Ok(&UserRole::User), SIGNER);
        assert_eq!(d.address, SIGNER);
        assert_eq!(d.warning, None);
    }

    /// ⚠ **THE PROBE MAY NEVER TURN A WORKING MOUNT INTO A PAPER ONE.** Every failure arm returns
    /// the address the old code returned, so the worst this change can do is warn. Asserted over all
    /// three failure shapes at once, because a single arm getting it right proves nothing about the
    /// others.
    #[test]
    fn every_failure_falls_back_to_todays_behaviour_and_says_so() {
        let network = hl_master_from_role(Err("connection reset".to_string()), SIGNER);
        assert_eq!(network.address, SIGNER, "a failed probe must not move the mount");
        assert!(network.warning.as_deref().unwrap().contains("connection reset"), "{network:?}");

        let missing = hl_master_from_role(Ok(&UserRole::Missing), SIGNER);
        assert_eq!(missing.address, SIGNER);
        assert!(missing.warning.as_deref().unwrap().contains("EXPIRED"), "{missing:?}");

        let unknown = hl_master_from_role(Ok(&UserRole::Unknown("multiSigUser".into())), SIGNER);
        assert_eq!(unknown.address, SIGNER);
        assert!(unknown.warning.is_some());
    }

    /// A role that NAMES a parent is believed even when this build does not expect it there — a
    /// sub-account address is still a better book than the signing key, and the warning says the
    /// classification was unexpected rather than swallowing it.
    #[test]
    fn a_named_parent_is_used_even_from_an_unexpected_role() {
        let role = UserRole::SubAccount { master: MASTER.into() };
        let d = hl_master_from_role(Ok(&role), SIGNER);
        assert_eq!(d.address, MASTER);
        assert!(d.warning.is_some(), "…but the shape was not what a signing key should answer");
    }

    /// The cure an operator pastes is the LABELLED key for a labelled account — telling them to
    /// write the unlabelled one would send them to configure a different account.
    #[test]
    fn the_cure_names_the_labelled_key_for_a_labelled_account() {
        assert!(hl_address_cure(&AccountLabel::Default).contains("ACCOUNT_ADDRESS`"));
        let alt = AccountLabel::parse("ALT").unwrap();
        assert!(
            hl_address_cure(&alt).contains("ACCOUNT_ADDRESS__ALT"),
            "{}",
            hl_address_cure(&alt)
        );
    }

    // ── AUDITING AN ADDRESS THE OPERATOR WROTE ───────────────────────────────────────────────
    //
    // `hl_audit_configured` is the pure half of the arm that used to trust a configured
    // `HYPERLIQUID_{TIER}_ACCOUNT_ADDRESS` without asking anybody. Every arm is driven here, because
    // the emission around it needs a live `userRole` round trip and is reachable by no test.

    /// **THE ONE THIS EXISTS FOR: the venue names a different account than the operator wrote.**
    ///
    /// Until 2026-09-20 nothing in this tree compared those two values, so a stale or mistyped
    /// address meant every read for this account answered about somebody else's book — and
    /// Hyperliquid answers a wrong address with `accountValue: "0.0"` rather than an error, so it
    /// read as FLAT and HEALTHY rather than broken.
    #[test]
    fn a_configured_address_the_venue_contradicts_is_a_disagreement() {
        let role = UserRole::Agent { master: MASTER.into() };
        let decided = hl_master_from_role(Ok(&role), SIGNER);
        let stranger = "0x2222222222222222222222222222222222222222";
        assert_eq!(
            hl_audit_configured(&decided, stranger),
            ConfiguredAudit::Disagrees { venue_said: MASTER.to_string() },
            "the venue's answer is carried so the report can name both sides"
        );
    }

    /// …and the agreeing case is `Confirms`, which is what records the handshake and moves the
    /// account off `NEVER VERIFIED`. Without this the test above would pass against a rule that
    /// called every configured address wrong.
    #[test]
    fn a_configured_address_the_venue_agrees_with_confirms() {
        let role = UserRole::Agent { master: MASTER.into() };
        let decided = hl_master_from_role(Ok(&role), SIGNER);
        assert_eq!(hl_audit_configured(&decided, MASTER), ConfiguredAudit::Confirms);
    }

    /// ⚠ **CASE IS NOT A DISAGREEMENT**, and this is the test that keeps the alarm worth reading.
    ///
    /// An EVM address is the same account in any case: the operator types it however their wallet
    /// printed it (checksummed, mixed case) and Hyperliquid answers lowercase. A raw comparison
    /// would raise the alarm on EVERY BOOT of a perfectly correct box, which is exactly how an
    /// operator learns to ignore a whole family of messages.
    #[test]
    fn a_differently_cased_address_is_the_same_account() {
        let role = UserRole::Agent { master: MASTER.to_ascii_uppercase() };
        let decided = hl_master_from_role(Ok(&role), SIGNER);
        assert_eq!(
            hl_audit_configured(&decided, MASTER),
            ConfiguredAudit::Confirms,
            "0xABC and 0xabc are one account"
        );
        // …and whitespace an operator pasted in is trimmed on the same rule.
        assert_eq!(
            hl_audit_configured(&decided, &format!("  {MASTER}  ")),
            ConfiguredAudit::Confirms
        );
    }

    /// **A PLAIN MASTER KEY CONFIRMS ITS OWN ADDRESS.** `UserRole::User` means *the key IS the
    /// account*, so `hl_master_from_role` answers the signer — and an operator who wrote that same
    /// address has written something correct, if redundant.
    #[test]
    fn a_plain_key_confirms_a_configured_address_equal_to_its_own() {
        let decided = hl_master_from_role(Ok(&UserRole::User), SIGNER);
        assert_eq!(hl_audit_configured(&decided, SIGNER), ConfiguredAudit::Confirms);
    }

    /// ⚠ **…and a plain key configured to a DIFFERENT address is a real finding, not a shrug.**
    /// The venue says this key acts only for itself, so an address pointing elsewhere is one the
    /// key cannot act for — the mount would read a book it can place no order against.
    #[test]
    fn a_plain_key_configured_elsewhere_disagrees() {
        let decided = hl_master_from_role(Ok(&UserRole::User), SIGNER);
        assert_eq!(
            hl_audit_configured(&decided, MASTER),
            ConfiguredAudit::Disagrees { venue_said: SIGNER.to_string() }
        );
    }

    /// **AN UNANSWERED PROBE SAYS NOTHING**, on every outcome that carries a warning: a network
    /// failure, a lapsed approval (`missing`), a role this build does not classify.
    ///
    /// ⚠ It must NOT read as a disagreement. On those arms `hl_master_from_role` falls back to the
    /// signer's own address, which is today's behaviour and NOT an observation — comparing a
    /// configured address against a fallback would raise the alarm every time the venue was
    /// briefly unreachable, on a box where nothing is wrong.
    #[test]
    fn an_unanswered_probe_is_not_evidence_against_a_configured_address() {
        let stranger = "0x2222222222222222222222222222222222222222";
        for decided in [
            hl_master_from_role(Err("connection reset".to_string()), SIGNER),
            hl_master_from_role(Ok(&UserRole::Missing), SIGNER),
            hl_master_from_role(Ok(&UserRole::Unknown("multiSigUser".into())), SIGNER),
            hl_master_from_role(Ok(&UserRole::Vault), SIGNER),
        ] {
            assert!(decided.warning.is_some(), "premise: this outcome warns");
            assert_eq!(
                hl_audit_configured(&decided, stranger),
                ConfiguredAudit::Unanswered,
                "a fallback is not an observation: {decided:?}"
            );
        }
    }
}
