//! Hyperliquid's VENUE ADAPTER — the `vike_bridge_core::venue_adapter::VenueAdapter` contract
//! ([`HyperliquidVenueAdapter`]) over the signer + transport wiring, the venue's own `userRole`
//! identity probe, and the exec + recon client construction
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md).
//!
//! The builder half moved down from `vike-tradehub-core`'s `hyperliquid` module on decision 0088's B3
//! (`docs/decisions/0088-mount-sheds-venue-facts-to-their-bridges.md`); the rest of what
//! `vike-tradehub-core` held for this venue — its `("hyperliquid", _)` arm, its arming-probe row, its
//! clock read, and its book-identity and grid-source rows — moved here with the contract, and
//! that module is gone.
//!
//! # The account's tier decides the network
//!
//! `live` means mainnet (docs/decisions/0095-venues-read-no-environment-and-live-means-mainnet.md):
//! [`AdapterInputs::tier`] `live` selects mainnet, and every lower tier testnet, by a `match` on the
//! tier itself — [`config::Network::for_tier`]. The key
//! is the started ACCOUNT's own `PRIVATE_KEY` field ([`AdapterInputs::credentials`], decision 0156
//! item 6), and that account HAS one tier, so its key signs on that tier's network and nowhere
//! else. There is no switch beside the tier, and a `live` account with no key does not run.
//!
//! # What this bridge hands back rather than does
//!
//! * **Recording what the venue said about this account.** The `userRole` probe runs here — it
//!   needs the `transport`/`signer` this module already builds, and a second transport would
//!   double this venue's per-IP REST spend (see `ip_gate_and_transport`). Parking the answer for
//!   `vike-cli secrets confirm` is `vike-tradehub-core`'s `record_confirmation`, which writes under the
//!   process's declared state directory — a process-global path no bridge reads — so a CONFIRMED answer leaves as the contract's
//!   `IdentityReport` and `vike-tradehub-core` performs the write.
//! * **Folding the grids.** The engine symbol's grid, its ruling margin mode and the declared
//!   legs' grids leave as `LiveExec` fields; `vike-tradehub-core`'s fold turns them into `RiskLimits`
//!   (including `grid_by_symbol`, through its own `declared_symbol_grids`) and the `Account`'s
//!   margin grid. The first two were `&mut` out-parameters of `live_runtime_for_account`, and
//!   `vike-tradehub-core`'s arm folded the legs' grids itself, until the contract gave all three a type.

use std::collections::HashMap;
use std::time::Duration;

use indexmap::IndexMap;
use vike_bridge_core::ratelimit::RateGate;
use vike_bridge_core::venue_adapter::{
    AdapterInputs, AdapterOutcome, AdapterRequest, BookIdentity, ClockAuth, ClockDecl, ClockRisk,
    DeclaredGridSource, ExecOutcome, IdentityReport, LiveExec, PaperCause, Resolution,
    VenueAdapter, VenueDeclaration, missing_time_field,
};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_credentials::AccountCredentials;

use crate::base::config;
use crate::base::symbology::Symbology;
use crate::base::transport::HyperliquidTransport;
use crate::feeds::user_role::UserRole;
use crate::signing::action::HlBuilderFee;

/// FALLBACK Hyperliquid BTC-perp grid (szDecimals-5 → tick 0.1 / step 1e-5, $10 min notional),
/// used ONLY if the adapter's startup `meta` fetch fails. The live client fetches the real grid.
///
/// Its only caller is [`live_runtime_for_account`], which is the reason it followed that function
/// down from `vike-tradehub-core` rather than joining the general table in vike-tradehub-core's `fallback.rs` —
/// whose own module doc had carved this one out ("Hyperliquid's fallback grid lives next to its
/// live-client helper… not here") before the okx port deleted that file with its last grid — and
/// the caller it sits beside is now this one.
fn fallback_properties() -> vike_model::SymbolProperties {
    vike_model::SymbolProperties {
        tick_size: 0.1,
        step_size: 0.00001,
        min_qty: 0.00001,
        max_qty: 1_000_000.0,
        min_notional: 10.0,
        // Everything else absent: HL sizes in the base asset (no contract multiplier, = 1.0), the
        // fallback never invents tiers (the scalar tick IS the grid), and HL declares no venue
        // hold. Functional-update syntax so a future `SymbolProperties` field costs this site
        // nothing — the reasoning vike-tradehub-core's since-deleted `fallback.rs` gave for its own grids.
        ..Default::default()
    }
}

/// The engine symbol's RULING margin mode, read off the venue's own `meta` — the mode a position
/// on this asset OPENS IN when the order names none.
///
/// `pub` (rather than the `pub(crate)` most of this module's pieces stay) because
/// `crates/vike-gatewayhub/tests/trade_roster/mount_roster/fee_schedule.rs`'s
/// `an_isolated_only_asset_folds_isolated_through_the_real_start_chain` feeds this function's answer
/// through `vike-tradehub-core`'s `margin_mode_grid` into an `Account` — the derivation and the fold joined
/// by the REAL function, which that test can only reach across the crate boundary (it lived in
/// `vike-tradehub-core` until the start contract finished and moved beside the other tests that name a
/// bridge). The link between the two inside a real start (the value riding `RuntimeFacts` into
/// [`LiveRuntime::margin_mode`] and on as `LiveExec::margin_mode`) is `adapter_contract_tests.rs`'s.
///
/// It is a THIN wrapper on purpose: the derivation belongs to this bridge
/// ([`crate::base::symbology::InstrumentRef::effective_margin_mode`], #1087 — `only_isolated` narrowing
/// [`vike_venues::VenueCaps::default_margin_mode`]), and duplicating the rule at the caller would be
/// the second source of truth that PR deliberately avoided creating.
///
/// A symbol the venue does not list falls back to the per-VENUE declaration, not to a literal: an
/// unlisted symbol has no per-asset narrowing to apply, which is exactly what that field means.
pub fn margin_mode(symbology: &Symbology, symbol: &str) -> vike_model::MarginMode {
    symbology
        .by_symbol(symbol)
        .map(|i| i.effective_margin_mode())
        .unwrap_or_else(|| vike_venues::caps_for(crate::base::consts::VENUE).default_margin_mode)
}

/// The ONE per-IP REST weight window a Hyperliquid start opens, and a transport already riding it.
///
/// Hyperliquid meters `/info` reads and signed `/exchange` actions out of a SINGLE per-IP budget
/// (`vike_venues::venue_rate_limits::HYPERLIQUID`'s `rest_ip_weight`), so the number of `RateGate`s
/// a start builds IS the multiple of that budget the process is willing to spend — and that row
/// admits exactly what the venue publishes, holding nothing back to absorb a second one. Every
/// [`HyperliquidTransport::new`] mints a FRESH gate, which is the whole reason this function
/// exists: it resolves the gate once ([`crate::base::ratelimit::ip_weight_gate`]) and hands back both
/// halves [`live_runtime_for_account`] needs — the handle to clone into the exec spawn (which clones
/// it on into the funding poller), and a transport wired to that same window for the instruments
/// load and the `HyperliquidReconClient` that inherits it. Three REST consumers, one budget.
///
/// Split out of [`live_runtime_for_account`] so the sharing is provable with no key, no signer and no
/// network: `an_hl_adapters_rest_paths_ride_one_ip_weight_window` takes the budget through one half
/// and watches it be gone from the other.
fn ip_gate_and_transport(network: config::Network) -> (RateGate, HyperliquidTransport) {
    ip_gate_and_transport_in(&ADAPTER_WINDOWS, network)
}

/// The newest per-IP window each network's adapter start opened (`WindowBook::opened`), so a
/// read made OUTSIDE a start ([`adapter_window_transport`]) charges the budget the adapter already spends.
struct WindowBook {
    newest: std::sync::Mutex<Vec<(config::Network, RateGate)>>,
}

static ADAPTER_WINDOWS: WindowBook = WindowBook { newest: std::sync::Mutex::new(Vec::new()) };

impl WindowBook {
    fn opened(&self, network: config::Network, gate: &RateGate) {
        let mut newest = self.newest.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match newest.iter_mut().find(|(n, _)| *n == network) {
            Some((_, held)) => *held = gate.clone(),
            None => newest.push((network, gate.clone())),
        }
    }

    fn newest(&self, network: config::Network) -> Option<RateGate> {
        let newest = self.newest.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        newest.iter().find(|(n, _)| *n == network).map(|(_, gate)| gate.clone())
    }
}

fn ip_gate_and_transport_in(
    book: &WindowBook,
    network: config::Network,
) -> (RateGate, HyperliquidTransport) {
    let ip_gate = crate::base::ratelimit::ip_weight_gate();
    book.opened(network, &ip_gate);
    let transport = HyperliquidTransport::new(network).with_rate_gate(ip_gate.clone());
    (ip_gate, transport)
}

/// A transport for a read made OUTSIDE an adapter start (the leverage confirmation,
/// [`crate::leverage_cap`]) that rides the per-IP window the newest adapter start of `network` opened,
/// so its weight is charged to the budget the adapter spends; with no start yet it opens one the way a
/// start does. ⚠ Not a process-wide window: each adapter start still opens its own (as before), so this
/// shares the NEWEST one, which is the start the read follows.
pub(crate) fn adapter_window_transport(network: config::Network) -> HyperliquidTransport {
    adapter_window_transport_in(&ADAPTER_WINDOWS, network)
}

fn adapter_window_transport_in(
    book: &WindowBook,
    network: config::Network,
) -> HyperliquidTransport {
    match book.newest(network) {
        Some(gate) => HyperliquidTransport::new(network).with_rate_gate(gate),
        None => ip_gate_and_transport_in(book, network).1,
    }
}

/// **The venue's own answer to "which account does this key trade" — and whether it is worth
/// recording.**
///
/// [`MasterOutcome::confirmed`] is `true` exactly when the `userRole` probe CONFIRMED
/// [`MasterOutcome::address`] — either the venue agreed the key IS the account, or it named the
/// master an agent key signs for. `false` covers every fallback arm (network failure, a lapsed
/// approval, a role this build does not classify, or a configured address the venue contradicted):
/// [`MasterOutcome::address`] is still today's USABLE fallback in every one of those cases, but an
/// unanswered or contradicted probe is not
/// evidence worth parking, so the caller must not record it.
///
/// The consumer is `outcome_from_attempt`, which turns a `confirmed` outcome into the contract's
/// `IdentityReport`; `vike-tradehub-core` then performs the write (`record_confirmation`) — one this bridge
/// cannot perform itself, since that function is `vike-tradehub-core`'s and writes under the process's
/// declared state directory, a process-global path no bridge reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MasterOutcome {
    pub address: String,
    pub confirmed: bool,
}

/// A live Hyperliquid exec + recon runtime, and the venue facts the fold needs from it — everything
/// the ONE instruments load resolved, handed back as values.
pub struct LiveRuntime {
    pub client: Box<dyn vike_exec::ExecutionClient + Send>,
    pub recon: Box<dyn vike_exec::recon::ReconClient>,
    /// The engine symbol's grid: the venue's own, or this module's fallback grid when `meta`
    /// does not list the symbol. Always present on a live start (was the `limits` out-parameter).
    pub grid: vike_model::SymbolProperties,
    /// The engine symbol's RULING margin mode — see [`margin_mode`] (was `margin_mode_out`).
    pub margin_mode: vike_model::MarginMode,
    /// Per-DECLARED-LEG `SymbolProperties`, resolved from the SAME instrument fetch — no second
    /// round trip. A leg the venue's `meta`/`spotMeta` does not list is simply absent. Handed on
    /// in insertion (= declaration) order as `LiveExec::leg_grids`.
    pub declared_leg_properties: IndexMap<String, vike_model::SymbolProperties>,
}

/// The result of attempting a live Hyperliquid start for one account.
///
/// [`LiveRuntimeAttempt::live`] and [`LiveRuntimeAttempt::master`] are carried SEPARATELY, rather than
/// folded into one `Option`, because they can resolve independently: the `userRole` probe runs
/// BEFORE the instruments fetch that [`LiveRuntimeAttempt::live`] also depends on, so a
/// `meta`/`spotMeta` failure can leave [`LiveRuntimeAttempt::master`] `Some` (the identity handshake
/// succeeded) while [`LiveRuntimeAttempt::live`] is `None` (the
/// start still falls back to paper). Splitting them is what lets the caller keep recording an
/// account confirmation exactly where today's start does, rather than only on a fully successful
/// start.
pub(crate) struct LiveRuntimeAttempt {
    pub live: Option<LiveRuntime>,
    pub master: Option<MasterOutcome>,
}

impl LiveRuntimeAttempt {
    fn none() -> Self {
        Self { live: None, master: None }
    }
}

/// Every venue fact a live start carries, read off the ONE instruments load — PURE over it, so
/// `adapter_contract_tests.rs` pins each value [`LiveRuntime`] hands on from a `Symbology::from_meta`
/// universe, with no key and no network. [`live_runtime_for_account`] only builds the two clients
/// around it; what stays unproven offline is that it hands [`RuntimeFacts::of`] its own
/// `instruments`, symbol and legs, because that call runs only after a live `meta` fetch.
struct RuntimeFacts {
    /// The engine symbol's grid — the venue's own, or [`fallback_properties`] when `meta` does not
    /// list the symbol.
    grid: vike_model::SymbolProperties,
    /// The engine symbol's RULING margin mode — see [`margin_mode`].
    margin_mode: vike_model::MarginMode,
    /// What the reconcile client's balance endpoint follows (perp `clearinghouseState` vs spot
    /// `spotClearinghouseState`): the engine symbol's product, perp when `meta` does not list it.
    product: config::Product,
    /// The declared legs the venue lists — see [`LiveRuntime::declared_leg_properties`].
    declared_leg_properties: IndexMap<String, vike_model::SymbolProperties>,
}

impl RuntimeFacts {
    fn of(
        instruments: &crate::feeds::instruments::HyperliquidInstruments,
        symbol: &str,
        declared_legs: &[String],
    ) -> Self {
        let grid = instruments.properties(symbol).copied().unwrap_or_else(fallback_properties);
        // The DECLARED LEGS' own properties, out of the SAME already-loaded `instruments` — no
        // second round trip. Deliberately NOT falling back to `fallback_properties` the way the
        // engine symbol does: that fallback is a venue-wide guess that happens to be right for the
        // engine market, and stamping it onto a leg would declare a grid nobody resolved. An
        // unknown leg gets no row, and `vike-tradehub-core`'s `symbol_grid::warn_ungridded_legs` reports it
        // once the caller folds this map.
        let mut declared_leg_properties = IndexMap::new();
        for leg in declared_legs {
            if let Some(p) = instruments.properties(leg) {
                declared_leg_properties.insert(leg.clone(), *p);
            }
        }
        let product = instruments
            .symbology()
            .by_symbol(symbol)
            .map(|i| i.product)
            .unwrap_or(config::Product::Perp);
        // The OTHER mount-time per-asset fact, from the same fetched `Symbology`: the mode a
        // position on this asset opens in (see [`margin_mode`]). Resolved HERE and nowhere else — a
        // paper outcome carries no margin mode, so the fold keeps `Cross`.
        let ruling_margin_mode = margin_mode(instruments.symbology(), symbol);
        RuntimeFacts { grid, margin_mode: ruling_margin_mode, product, declared_leg_properties }
    }

    /// The live start these facts describe, around its two clients — the ONE place a fact becomes a
    /// [`LiveRuntime`] field.
    fn into_live_runtime(
        self,
        client: Box<dyn vike_exec::ExecutionClient + Send>,
        recon: Box<dyn vike_exec::recon::ReconClient>,
    ) -> LiveRuntime {
        LiveRuntime {
            client,
            recon,
            grid: self.grid,
            margin_mode: self.margin_mode,
            declared_leg_properties: self.declared_leg_properties,
        }
    }
}

/// Build Hyperliquid's live exec client (+ its reconcile handle) from the account's bespoke fields
/// (`creds`: a secp256k1 `PRIVATE_KEY` + an optional master `ACCOUNT_ADDRESS` — NOT the standard
/// API_KEY/SECRET, so they are loaded here, by [`config::load`], rather than via
/// `vike_secrets::credentials::api_credentials`). [`LiveRuntimeAttempt::live`] is `None`
/// when the key is absent (the live gate) or any startup step fails (bad key / meta fetch) → the
/// caller falls back to paper. On success [`LiveRuntime`] carries the fetched grid and margin mode.
///
/// `tier` is the account's tier [`HyperliquidVenueAdapter`] was handed: `live` is mainnet, every
/// lower tier testnet ([`config::Network::for_tier`], decision 0095); this function reads no
/// environment. `vars` is the deployment's NAMED values (`AdapterInputs::secrets`): the folded HIP-3
/// flag, the attribution code and the builder-fee knob — never a credential.
///
/// The `HyperliquidReconClient` is built HERE rather than through a generic recon factory because
/// HL's keyless-`/info`-against-master report reads need the bespoke [`HyperliquidTransport`]/master
/// this function already builds, not a standard `Credentials`-shaped factory. It reads on the SAME
/// network the exec trades so recon sees the account the orders actually land in; its `Product`
/// (perp vs spot balance endpoint) follows the engine symbol's.
///
/// `market_slippage` is the DEPLOYMENT's requested emulated-market aggression band
/// (`policy.toml`'s `market_slippage`, projected by the binary and threaded through unchanged).
/// Hyperliquid has no native market order: a market intent is priced as an `Ioc` limit at
/// `mid * (1 ± band)` and a tripped stop-MARKET at `trigger * (1 ± band)`, so this band is the WORST
/// price such an order is allowed to reach. It is resolved ONCE here, through
/// [`crate::exec::market_slippage_for`] — which returns the adapter's own historical literal on
/// `None` (byte-identical to every start before this parameter existed) and CLAMPS an out-of-range
/// value into the workspace bounds.
///
/// `declared_legs`' own grids come out of the SAME already-loaded instrument fetch — see
/// [`LiveRuntime::declared_leg_properties`].
///
/// `halt_path` is the operator HALT sentinel the exec client watches
/// (`AdapterInputs::process.halt_path`, the one file `vike-tradehub-core` resolved for the whole process),
/// handed to the client with `HyperliquidExecutionClient::with_halt_path`: this function resolves
/// no process-global path (decision 0099).
pub(crate) fn live_runtime_for_account(
    symbol: &str,
    declared_legs: &[String],
    tier: AccountTier,
    creds: &AccountCredentials<'_>,
    vars: &HashMap<String, String>,
    label: &AccountLabel,
    live_events: &vike_exec::EventSender,
    recon_trigger: Option<std::sync::mpsc::Sender<()>>,
    market_slippage: Option<f64>,
    halt_path: std::path::PathBuf,
) -> LiveRuntimeAttempt {
    let mainnet = tier == AccountTier::Live;
    // ⚠ The started ACCOUNT's own fields — the one input that keeps a labelled account from
    // signing with the DEFAULT account's private key: `vike-tradehub-core` hands each account its own.
    let Some(creds) = config::load(tier, creds) else {
        return LiveRuntimeAttempt::none(); // absent key = the live gate → paper
    };
    let signer = match crate::signing::Signer::from_private_key(&creds.private_key, creds.network) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("hyperliquid: invalid private key ({e}); staying paper");
            return LiveRuntimeAttempt::none();
        }
    };
    // ONE per-IP REST weight window for every REST path this start opens — the instruments load and
    // the recon reads through `transport`, the exec thread's `/exchange` and the funding poller's
    // `/info` through `ip_gate`. See [`ip_gate_and_transport`] for why a per-consumer gate is a
    // multiple of the venue's cap rather than a conservative default.
    let (ip_gate, transport) = ip_gate_and_transport(creds.network);
    // ⚠ **WHOSE BOOK THIS KEY TRADES, ASKED OF THE VENUE RATHER THAN ASSUMED.** See
    // [`MasterOutcome`]'s doc for what `confirmed` means and why recording it is the caller's job.
    let master_outcome = match creds.account_address.clone() {
        Some(addr) => audit_configured_master(&transport, signer.address(), label, addr),
        None => resolve_master(&transport, signer.address(), label),
    };
    let master = master_outcome.address.clone();

    // ⚠ `load_from_vars`, not `load`: the HIP-3 universe opt-in is `flags.hyperliquid_hip3`, folded
    // into this same `vars` map by the composition root — the map is its only source (decision 0095
    // retired the variable, which refuses startup now).
    let instruments = match crate::feeds::instruments::HyperliquidInstruments::load_from_vars(
        &transport, None, vars,
    ) {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!("hyperliquid: meta/spotMeta load failed ({e}); staying paper");
            // The identity probe above already ran and may already be worth recording — that does
            // not depend on whether the start goes on to succeed.
            return LiveRuntimeAttempt { live: None, master: Some(master_outcome) };
        }
    };
    // Every venue fact this start carries, off the SAME `instruments` — see [`RuntimeFacts`].
    let facts = RuntimeFacts::of(&instruments, symbol, declared_legs);
    // Reconcile handle: reuse the SAME transport (moved in — the instruments load above only
    // borrowed it) + master this function already built; keyless `/info` reads against the master on
    // `creds.network`, its balance endpoint following the engine symbol's product.
    let recon: Box<dyn vike_exec::recon::ReconClient> =
        crate::recon_client(transport, master.clone(), facts.product);
    // Two lines, not one with substituted words: `⚠ REAL-MONEY: ` marks the live tier and nothing else
    // (`crates/vike-ops/tests/venues/live_mount_line_gate.rs`).
    if mainnet {
        tracing::warn!(
            venue = crate::base::consts::VENUE,
            account = %label,
            tier = AccountTier::Live.as_str(),
            "⚠ REAL-MONEY: hyperliquid: LIVE credentials present → LIVE exec client (real MAINNET orders)"
        );
    } else {
        tracing::warn!(
            venue = crate::base::consts::VENUE,
            account = %label,
            tier = AccountTier::Demo.as_str(),
            "hyperliquid: DEMO credentials present → LIVE exec client (real testnet orders)"
        );
    }
    // Builder Codes (task 7, unified-venue-attribution): resolved ONCE from the workspace `.env`.
    // `attribution_code_from` validates the address against `hyperliquid`'s
    // `AttributionMechanic::SignedBuilder`; absent/invalid degrades to `None` (byte-identical — no
    // `builder` object on the wire at all, matching every start before this field existed). The
    // fee defaults to 0 (attribution-only, no on-chain `approveBuilderFee` needed — see
    // `crate::user_signed::builder_fee`'s module doc) unless `HYPERLIQUID_BUILDER_FEE_TENTHS_BP` is set AND the
    // operator has separately run the one-time approval from the master wallet.
    let builder =
        vike_secrets::credentials::attribution_code_from(vars, "hyperliquid").map(|address| {
            let fee_tenths_bp = vars
                .get("HYPERLIQUID_BUILDER_FEE_TENTHS_BP")
                .and_then(|s| s.trim().parse::<u32>().ok())
                .unwrap_or(0);
            HlBuilderFee { address: address.trim().to_ascii_lowercase(), fee_tenths_bp }
        });
    // The emulated-market band (Phase 6c), resolved ONCE here and carried into the exec thread.
    // `market_slippage_for(None)` IS `DEFAULT_MARKET_SLIPPAGE`, so an unconfigured start is
    // byte-identical to the plain `spawn` this call replaced. Logged only when the operator
    // actually expressed a value, so the default path's log output is unchanged too — and a
    // clamped value has already been warned about by `market_slippage_for` itself.
    let applied_slippage = crate::exec::market_slippage_for(market_slippage);
    if market_slippage.is_some() {
        tracing::info!(
            requested = ?market_slippage,
            applied = applied_slippage,
            "hyperliquid: emulated-market slippage band set from policy"
        );
    }
    let client: Box<dyn vike_exec::ExecutionClient + Send> = Box::new(
        crate::HyperliquidExecutionClient::spawn_with_market_slippage(
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
        )
        .with_halt_path(halt_path),
    );
    LiveRuntimeAttempt {
        live: Some(facts.into_live_runtime(client, recon)),
        master: Some(master_outcome),
    }
}

/// **Which account does this signing key trade for?** — asked of the venue, with every failure
/// falling back to the address the key derives.
///
/// The fallback is TODAY'S behaviour, so nothing this function does can make a start worse than it
/// was; what it adds is knowing, and saying, when that fallback is a guess. Each arm names the cure,
/// which is the same one line in every case: write the account address beside the key.
///
/// ⚠ **Not `Result`.** A caller that had to handle an error here would have to decide whether an
/// identity probe failing should stop a start, and the answer is always no — see [`live_runtime_for_account`]'s
/// own note. Folding the decision in here keeps it from being re-made differently at a second call site.
pub(crate) fn resolve_master(
    transport: &HyperliquidTransport,
    signer_address: &str,
    label: &AccountLabel,
) -> MasterOutcome {
    let answer = crate::feeds::user_role::user_role(transport, signer_address);
    let decided = hl_master_from_role(answer.as_ref().map_err(|e| e.to_string()), signer_address);
    if let Some(warning) = &decided.warning {
        tracing::warn!(account = %label, address = %signer_address, "hyperliquid: {warning} {}", hl_address_cure(label));
        // ⚠ The caller must not record this: `warning.is_some()` means the venue did NOT answer —
        // a network failure, a lapsed approval, a role this build does not classify — and
        // `decided.address` is then today's FALLBACK rather than an observation.
        return MasterOutcome { address: decided.address, confirmed: false };
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
    // **THE VENUE ANSWERED, SO THE STORE CAN BE TOLD** — by the caller, which is the pair of arms
    // in which Hyperliquid itself classified the address: it named the master an agent signs for,
    // or it agreed the key IS the account.
    MasterOutcome { address: decided.address, confirmed: true }
}

/// **What the venue said about an address the OPERATOR wrote** — the verdict behind the audit of an
/// account's configured `ACCOUNT_ADDRESS` field.
///
/// A separate type from `vike_secrets::account_confirmation::Verdict` on purpose, because it compares
/// a different PAIR. That one compares the venue's answer against the `account` TABLE (a
/// `vike-tradehub-core`-only comparison); this one compares it against the CREDENTIAL STORE's setting — the
/// value that decides which address this start actually reads and trades. They can disagree
/// independently and mean different things.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ConfiguredAudit {
    /// The venue named exactly the account the operator wrote. Nothing is wrong and the handshake
    /// is worth recording — this is what stamps `account.last_verified_at`.
    Confirms,
    /// ⚠ **The venue named a DIFFERENT account.** The key cannot act for the address this start was
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
/// start.
///
/// ⚠ **Both sides are normalised** (trim + lowercase). An EVM address is the same account in any
/// case, the operator types it however their wallet printed it, and Hyperliquid answers lowercase —
/// so a raw comparison would raise [`ConfiguredAudit::Disagrees`] on every single boot of a
/// perfectly correct box. That is the one failure mode that would make an operator learn to ignore
/// this alarm.
fn hl_audit_configured(decided: &HlMaster, configured: &str) -> ConfiguredAudit {
    if decided.warning.is_some() {
        return ConfiguredAudit::Unanswered;
    }
    let said = normalize_address(&decided.address);
    if said == normalize_address(configured) {
        ConfiguredAudit::Confirms
    } else {
        ConfiguredAudit::Disagrees { venue_said: said }
    }
}

/// The canonical comparison form for an EVM address: trimmed, lowercased ASCII. Mirrors
/// `vike-tradehub-core`'s `book_identity::normalize_book`, kept as its own tiny copy here rather than a
/// cross-crate call — this bridge does not otherwise depend on `vike-tradehub-core`'s book-identity table,
/// and a bridge may not gain that dependency just to reuse a one-line string helper (the layer rule
/// runs the other way).
fn normalize_address(raw: &str) -> String {
    raw.trim().to_ascii_lowercase()
}

/// **Audit an address the operator WROTE, against the venue's own answer** — and return the address
/// this start will use, which is ALWAYS the configured one.
///
/// # ⚠ It audits; it does not re-route
///
/// The returned address is `configured`, unchanged, on every arm including
/// [`ConfiguredAudit::Disagrees`]. That is deliberate and it is the same posture
/// `vike_secrets::account_confirmation::verdict` takes for its own disagreement:
///
/// * **it does not switch to the venue's answer.** That would move which account this process reads
///   and trades, silently, on a boot the operator did not ask for — a routing change nobody
///   requested is worse than a wrong value they can be told about;
/// * **it does not refuse the start.** The session authenticated. Taking the venue away over a
///   bookkeeping disagreement degrades a capability that was working
///   (`docs/decisions/0013-degrade-vs-refuse.md`), and the credential schema's §8 rules this exact
///   case: *a violation discovered AT A HANDSHAKE is REPORTED, not thrown*;
/// * **it parks NOTHING on a disagreement** — `confirmed: false` — for the same reason `resolve_master`
///   does not record an unanswered probe: a disagreement is a finding about the CONFIGURED value, not
///   a confirmation of what the start is actually using.
///
/// # What it costs, and why that was a decision rather than an omission
///
/// One `userRole` request per hyperliquid start that has a configured address — weight 60 of a
/// shared 1200-per-minute per-IP budget, once, at startup. Until 2026-09-20 this path made NO
/// request at all and the account was never verified; `crate::feeds::user_role`'s module doc carries what
/// the weight means and why the probe may never sit in a loop.
///
/// What it buys is the one comparison nothing else in the tree performs. Every other check reads the
/// configured address and trusts it. If it is stale or mistyped, this start reads the WRONG
/// account — and Hyperliquid answers a wrong address with `accountValue: "0.0"` rather than an
/// error, so the result is a book that looks flat and healthy while reconcile finds nothing to
/// reconcile. That is precisely the failure `vike_secrets::account_confirmation` exists to remove, on
/// the one input that had no check at all.
fn audit_configured_master(
    transport: &HyperliquidTransport,
    signer_address: &str,
    label: &AccountLabel,
    configured: String,
) -> MasterOutcome {
    let answer = crate::feeds::user_role::user_role(transport, signer_address);
    let decided = hl_master_from_role(answer.as_ref().map_err(|e| e.to_string()), signer_address);
    let confirmed = match hl_audit_configured(&decided, &configured) {
        // ⚠ SILENT, and deliberately so. An unanswered probe is not evidence against a configured
        // value, and this arm is reachable by an ordinary network blip — a line here would teach an
        // operator that the family is noise. The warning `resolve_master` emits on the same outcome
        // is for the opposite case, where the fallback is all there is.
        ConfiguredAudit::Unanswered => false,
        // Nothing is WRONG, so nothing is warned — but the caller should still record the
        // handshake: this is the only thing that ever moves this account off `NEVER VERIFIED`.
        ConfiguredAudit::Confirms => true,
        ConfiguredAudit::Disagrees { venue_said } => {
            tracing::error!(
                account = %label,
                configured = %configured,
                venue_answered = %venue_said,
                "hyperliquid: ⚠ THE CONFIGURED ADDRESS AND THE VENUE DISAGREE. \
                 This account's `ACCOUNT_ADDRESS` field says `{configured}`, and \
                 the venue's own `userRole` says this key signs for `{venue_said}`. This mount is \
                 using the CONFIGURED address — nothing was re-routed and nothing was written, \
                 because switching accounts on a boot you did not ask for is worse than telling \
                 you. ⚠ If the configured address is wrong, every read for this account is \
                 answering about somebody else's book, and Hyperliquid answers a wrong address \
                 with `accountValue: \"0.0\"` rather than an error — so it looks FLAT and HEALTHY \
                 rather than broken. Resolve it once: either correct the key to `{venue_said}`, or \
                 re-approve this API wallet under the account you meant."
            );
            false
        }
    };
    MasterOutcome { address: configured, confirmed }
}

/// The field an operator would write to settle the question outright, and the account it belongs
/// to, spelled rather than described — an operator acts on a named field of a named account, not on
/// a sentence about one.
fn hl_address_cure(label: &AccountLabel) -> String {
    match label.text() {
        None => {
            "Set the default hyperliquid account's `ACCOUNT_ADDRESS` field to state it outright."
                .to_string()
        }
        Some(l) => {
            format!("Set hyperliquid account {l}'s `ACCOUNT_ADDRESS` field to state it outright.")
        }
    }
}

/// What the resolver decided: the address to start on, and the warning that should accompany it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HlMaster {
    address: String,
    /// `None` when the answer was CONFIRMED by the venue — either it named the master or it agreed
    /// the key is the account. Every other outcome carries the sentence that says so.
    warning: Option<String>,
}

/// **Role → the address to start on**, PURE.
///
/// Split from the I/O so the branching — which arm trusts the venue, which falls back, which warns —
/// is reachable from a test without a network, a key or a transport. That branching is the whole of
/// the risk here: every fallback returns TODAY'S behaviour, so a wrong arm is silent by construction.
fn hl_master_from_role(answer: Result<&UserRole, String>, signer_address: &str) -> HlMaster {
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
        // ⚠ The probe failed to ASK. Never a reason to refuse the start: that would trade the
        // venue's REST availability for this box's ability to start.
        Err(e) => fallback(format!(
            "could not ask the venue which account this key trades for ({e}), so the key's own \
             address is assumed to BE the account — true for a master key, WRONG for an API wallet, \
             where it reads an empty book."
        )),
    }
}

/// The contract's tier for a network: `live` on mainnet, `demo` on testnet — so a `paper` tier,
/// which never reaches a bridge and reaches testnet ([`config::Network::for_tier`]), binds as
/// `demo` does.
fn tier_of(network: config::Network) -> AccountTier {
    match network {
        config::Network::Mainnet => AccountTier::Live,
        config::Network::Testnet => AccountTier::Demo,
    }
}

/// `{"specialStatuses":null,"time":<epoch ms>}` — the `exchangeStatus` body. Split from the read
/// so the field is gated by a fixture test rather than by a live call.
fn parse_server_time(body: &serde_json::Value) -> Result<i64, String> {
    body.get("time").and_then(serde_json::Value::as_i64).ok_or_else(|| missing_time_field("time"))
}

/// A start attempt as the contract's outcome — PURE, so every field's fold is testable with no
/// key and no network (`adapter_contract_tests.rs`).
///
/// ⚠ The identity is carried on EVERY outcome the probe confirmed, the PAPER one included: the
/// `userRole` probe runs before `meta`, so a `meta` failure can follow a confirmed answer, and
/// `vike-tradehub-core`'s old arm recorded it anyway. Its tier is the NETWORK the handshake ran on, never
/// the tier in scope.
fn outcome_from_attempt(attempt: LiveRuntimeAttempt, network: config::Network) -> AdapterOutcome {
    let tier = tier_of(network);
    let identity = attempt.master.filter(|m| m.confirmed).map(|m| IdentityReport {
        book: m.address,
        evidence: "`userRole`",
        tier,
    });
    let Some(live) = attempt.live else {
        return AdapterOutcome { exec: ExecOutcome::Paper, recon: None, identity };
    };
    AdapterOutcome {
        exec: ExecOutcome::Live(LiveExec {
            client: live.client,
            bound_tier: tier,
            grid: Some(live.grid),
            contract_value: None,
            margin_mode: Some(live.margin_mode),
            leg_grids: live.declared_leg_properties.into_iter().collect(),
        }),
        recon: Some(live.recon),
        identity,
    }
}

/// hyperliquid's start. `vike_tradehub::registry::REGISTRY` holds `&HyperliquidVenueAdapter`.
pub struct HyperliquidVenueAdapter;

impl VenueAdapter for HyperliquidVenueAdapter {
    fn venue(&self) -> &'static str {
        crate::base::consts::VENUE
    }

    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration {
            addresses_accounts: true,
            process_exclusive: None,
            // HL's exec pump fires the recon trigger on every WS reconnect.
            takes_recon_trigger: true,
            // `live_runtime_for_account` loads `HyperliquidInstruments` for the WHOLE venue before it
            // resolves the engine symbol's properties, so every other asset is already in hand.
            grid_source: DeclaredGridSource::InHand,
            // `crate::base::config`'s `load`: the account's `ACCOUNT_ADDRESS` field is the MASTER
            // address when the key is an agent wallet, and absent means "the key IS the account" —
            // derivable offline from the EVM key in its `PRIVATE_KEY` field. Sub-accounts have their
            // own addresses, so two of them are two books.
            book_identity: BookIdentity::Named {
                name_fields: &[config::ACCOUNT_ADDRESS],
                evm_key_fields: &[config::PRIVATE_KEY],
            },
            clock: ClockDecl::Wired {
                endpoint: "POST /info {\"type\":\"exchangeStatus\"} (public)",
                auth: ClockAuth::Public,
                risk: ClockRisk::NonceWindow,
            },
        }
    }

    /// The account's key arms its tier's network — INTENT, not outcome: a present key
    /// that will not parse still arms, so the budget refusal fires before any session, and
    /// [`Self::start`] declines it into paper. `held_below_live` is always `None`: testnet is
    /// reached only under a tier below `live`.
    ///
    /// No key is paper, and under a `live` tier the cause is the LIVE key's absence: the account
    /// holds ONE key set, so there is no testnet key beside it, and mainnet is never signed with one.
    fn resolve(&self, inputs: &AdapterInputs<'_>) -> Resolution {
        let tier = inputs.tier;
        if config::load(tier, &inputs.credentials).is_some() {
            Resolution::Armed {
                tier: tier_of(config::Network::for_tier(tier)),
                held_below_live: None,
            }
        } else if tier == AccountTier::Live {
            Resolution::Paper(PaperCause::LiveCredentialsAbsent)
        } else {
            Resolution::Paper(PaperCause::NoCredentials)
        }
    }

    fn start(&self, req: AdapterRequest<'_>) -> AdapterOutcome {
        let tier = req.inputs.tier;
        // THE POLICY CONSUMER: HL is the one roster venue with no native market order, so
        // `req.market_slippage` binds here and nowhere else (`crate::exec`'s
        // `market_slippage_for` resolves `None` to the adapter's historical literal).
        let attempt = live_runtime_for_account(
            req.symbol,
            req.declared_legs,
            tier,
            &req.inputs.credentials,
            req.inputs.secrets,
            req.inputs.account,
            req.events,
            req.recon_trigger,
            req.market_slippage,
            req.inputs.process.halt_path.clone(),
        );
        outcome_from_attempt(attempt, config::Network::for_tier(tier))
    }

    /// `POST /info {"type":"exchangeStatus"}` — keyless, over the crate's OWN transport on an
    /// agent bounded by `timeout`, on the network the tier selects. The two networks' clocks
    /// genuinely differ, so this must not be simplified to "always mainnet".
    fn server_time_ms(&self, inputs: &AdapterInputs<'_>, timeout: Duration) -> Result<i64, String> {
        let body = HyperliquidTransport::with_agent(
            config::Network::for_tier(inputs.tier),
            vike_bridge_core::http::blocking_agent_with_timeout(timeout),
        )
        .info(&serde_json::json!({ "type": "exchangeStatus" }))
        .map_err(|e| e.to_string())?;
        parse_server_time(&body)
    }
}

#[path = "adapter_tests.rs"]
#[cfg(test)]
mod adapter_tests;

#[path = "master_resolution_tests.rs"]
#[cfg(test)]
mod master_resolution_tests;

#[path = "adapter_contract_tests.rs"]
#[cfg(test)]
mod adapter_contract_tests;
