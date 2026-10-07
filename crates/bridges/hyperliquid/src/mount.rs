//! Hyperliquid's VENUE MOUNT — the `vike_bridge_core::venue_mount::VenueMount` contract
//! ([`HyperliquidVenueMount`]) over the signer + transport wiring, the venue's own `userRole`
//! identity probe, and the exec + recon client construction
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md).
//!
//! The builder half moved down from `vike-mount`'s `hyperliquid` module on decision 0088's B3
//! (`docs/decisions/0088-mount-sheds-venue-facts-to-their-bridges.md`); the rest of what
//! `vike-mount` held for this venue — its `("hyperliquid", _)` arm, its arming-probe row, its
//! clock read, and its book-identity and grid-source rows — moved here with the contract, and
//! that module is gone.
//!
//! # The ceiling decides the network
//!
//! `live` means mainnet (docs/decisions/0095-venues-read-no-environment-and-live-means-mainnet.md):
//! [`MountInputs::live_permitted`] selects [`config::Env::Live`] — the LIVE key and mainnet — and
//! every lower ceiling [`config::Env::Demo`] — the DEMO key and testnet — through the one mapping,
//! [`config::Env::for_ceiling`]. There is no switch beside the ceiling, and a `live` ceiling with no
//! LIVE key stays paper.
//!
//! # What this bridge hands back rather than does
//!
//! * **Recording what the venue said about this account.** The `userRole` probe runs here — it
//!   needs the `transport`/`signer` this module already builds, and a second transport would
//!   double this venue's per-IP REST spend (see `ip_gate_and_transport`). Parking the answer for
//!   `vike-cli secrets confirm` is `vike-mount`'s `record_confirmation`, which takes a `VenueMode`
//!   — a type that never crosses into a bridge — so a CONFIRMED answer leaves as the contract's
//!   `IdentityReport` and `vike-mount` performs the write.
//! * **Folding the grids.** The mounted symbol's grid, its ruling margin mode and the declared
//!   legs' grids leave as `LiveExec` fields; `vike-mount`'s fold turns them into `RiskLimits`
//!   (including `grid_by_symbol`, through its own `declared_symbol_grids`) and the `Account`'s
//!   margin grid. The first two were `&mut` out-parameters of `live_mount_for_account`, and
//!   `vike-mount`'s arm folded the legs' grids itself, until the contract gave all three a type.

use std::collections::HashMap;
use std::time::Duration;

use indexmap::IndexMap;
use vike_bridge_core::ratelimit::RateGate;
use vike_bridge_core::venue_mount::{
    BookIdentity, ClockAuth, ClockDecl, ClockRisk, DeclaredGridSource, ExecOutcome, IdentityReport,
    LiveExec, MountInputs, MountOutcome, MountRequest, PaperCause, Resolution, Tier,
    VenueDeclaration, VenueMount, missing_time_field,
};
use vike_model::accounts::account_keys::AccountLabel;

use crate::config;
use crate::signing::action::HlBuilderFee;
use crate::symbology::Symbology;
use crate::transport::HyperliquidTransport;
use crate::user_role::UserRole;

/// FALLBACK Hyperliquid BTC-perp grid (szDecimals-5 → tick 0.1 / step 1e-5, $10 min notional),
/// used ONLY if the adapter's startup `meta` fetch fails. The live client fetches the real grid.
///
/// Its only caller is [`live_mount_for_account`], which is the reason it followed that function
/// down from `vike-mount` rather than joining the general table in vike-mount's `fallback.rs` —
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
        // nothing — the reasoning vike-mount's since-deleted `fallback.rs` gave for its own grids.
        ..Default::default()
    }
}

/// The mounted symbol's RULING margin mode, read off the venue's own `meta` — the mode a position
/// on this asset OPENS IN when the order names none.
///
/// `pub` (rather than the `pub(crate)` most of this module's pieces stay) because
/// `crates/vike-tradehub/tests/mount_roster/fee_schedule.rs`'s
/// `an_isolated_only_asset_folds_isolated_through_the_real_mount_chain` feeds this function's answer
/// through `vike-mount`'s `margin_mode_grid` into an `Account` — the derivation and the fold joined
/// by the REAL function, which that test can only reach across the crate boundary (it lived in
/// `vike-mount` until the mount contract finished and moved beside the other tests that name a
/// bridge). The link between the two inside a real mount (the value riding `MountFacts` into
/// [`LiveMount::margin_mode`] and on as `LiveExec::margin_mode`) is `mount_contract_tests.rs`'s.
///
/// It is a THIN wrapper on purpose: the derivation belongs to this bridge
/// ([`crate::symbology::InstrumentRef::effective_margin_mode`], #1087 — `only_isolated` narrowing
/// [`vike_model::VenueCaps::default_margin_mode`]), and duplicating the rule at the caller would be
/// the second source of truth that PR deliberately avoided creating.
///
/// A symbol the venue does not list falls back to the per-VENUE declaration, not to a literal: an
/// unlisted symbol has no per-asset narrowing to apply, which is exactly what that field means.
pub fn margin_mode(symbology: &Symbology, symbol: &str) -> vike_model::MarginMode {
    symbology
        .by_symbol(symbol)
        .map(|i| i.effective_margin_mode())
        .unwrap_or_else(|| vike_model::caps_for(crate::consts::VENUE).default_margin_mode)
}

/// The ONE per-IP REST weight window a Hyperliquid mount opens, and a transport already riding it.
///
/// Hyperliquid meters `/info` reads and signed `/exchange` actions out of a SINGLE per-IP budget
/// (`vike_model::venues::venue_rate_limits::HYPERLIQUID`'s `rest_ip_weight`), so the number of `RateGate`s
/// a mount builds IS the multiple of that budget the process is willing to spend — and that row
/// admits exactly what the venue publishes, holding nothing back to absorb a second one. Every
/// [`HyperliquidTransport::new`] mints a FRESH gate, which is the whole reason this function
/// exists: it resolves the gate once ([`crate::ratelimit::ip_weight_gate`]) and hands back both
/// halves [`live_mount_for_account`] needs — the handle to clone into the exec spawn (which clones
/// it on into the funding poller), and a transport wired to that same window for the instruments
/// load and the `HyperliquidReconClient` that inherits it. Three REST consumers, one budget.
///
/// Split out of [`live_mount_for_account`] so the sharing is provable with no key, no signer and no
/// network: `an_hl_mounts_rest_paths_ride_one_ip_weight_window` takes the budget through one half
/// and watches it be gone from the other.
fn ip_gate_and_transport(network: config::Network) -> (RateGate, HyperliquidTransport) {
    let ip_gate = crate::ratelimit::ip_weight_gate();
    let transport = HyperliquidTransport::new(network).with_rate_gate(ip_gate.clone());
    (ip_gate, transport)
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
/// `IdentityReport`; `vike-mount` then performs the write (`record_confirmation`) — one this bridge
/// cannot perform itself, since that function is `vike-mount`'s and takes a `VenueMode`, a type
/// that never crosses into a bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MasterOutcome {
    pub address: String,
    pub confirmed: bool,
}

/// A live Hyperliquid exec + recon mount, and the venue facts the fold needs from it — everything
/// the ONE instruments load resolved, handed back as values.
pub struct LiveMount {
    pub client: Box<dyn vike_exec::ExecutionClient + Send>,
    pub recon: Box<dyn vike_exec::recon::ReconClient>,
    /// The mounted symbol's grid: the venue's own, or this module's fallback grid when `meta`
    /// does not list the symbol. Always present on a live mount (was the `limits` out-parameter).
    pub grid: vike_model::SymbolProperties,
    /// The mounted symbol's RULING margin mode — see [`margin_mode`] (was `margin_mode_out`).
    pub margin_mode: vike_model::MarginMode,
    /// Per-DECLARED-LEG `SymbolProperties`, resolved from the SAME instrument fetch — no second
    /// round trip. A leg the venue's `meta`/`spotMeta` does not list is simply absent. Handed on
    /// in insertion (= declaration) order as `LiveExec::leg_grids`.
    pub declared_leg_properties: IndexMap<String, vike_model::SymbolProperties>,
}

/// The result of attempting a live Hyperliquid mount for one account.
///
/// [`LiveMountAttempt::live`] and [`LiveMountAttempt::master`] are carried SEPARATELY, rather than
/// folded into one `Option`, because they can resolve independently: the `userRole` probe runs
/// BEFORE the instruments fetch that [`LiveMountAttempt::live`] also depends on, so a
/// `meta`/`spotMeta` failure can leave [`LiveMountAttempt::master`] `Some` (the identity handshake
/// succeeded) while [`LiveMountAttempt::live`] is `None` (the
/// mount still falls back to paper). Splitting them is what lets the caller keep recording an
/// account confirmation exactly where today's mount does, rather than only on a fully successful
/// mount.
pub(crate) struct LiveMountAttempt {
    pub live: Option<LiveMount>,
    pub master: Option<MasterOutcome>,
}

impl LiveMountAttempt {
    fn none() -> Self {
        Self { live: None, master: None }
    }
}

/// Every venue fact a live mount carries, read off the ONE instruments load — PURE over it, so
/// `mount_contract_tests.rs` pins each value [`LiveMount`] hands on from a `Symbology::from_meta`
/// universe, with no key and no network. [`live_mount_for_account`] only builds the two clients
/// around it; what stays unproven offline is that it hands [`MountFacts::of`] its own
/// `instruments`, symbol and legs, because that call runs only after a live `meta` fetch.
struct MountFacts {
    /// The mounted symbol's grid — the venue's own, or [`fallback_properties`] when `meta` does not
    /// list the symbol.
    grid: vike_model::SymbolProperties,
    /// The mounted symbol's RULING margin mode — see [`margin_mode`].
    margin_mode: vike_model::MarginMode,
    /// What the reconcile client's balance endpoint follows (perp `clearinghouseState` vs spot
    /// `spotClearinghouseState`): the mounted symbol's product, perp when `meta` does not list it.
    product: config::Product,
    /// The declared legs the venue lists — see [`LiveMount::declared_leg_properties`].
    declared_leg_properties: IndexMap<String, vike_model::SymbolProperties>,
}

impl MountFacts {
    fn of(
        instruments: &crate::instruments::HyperliquidInstruments,
        symbol: &str,
        declared_legs: &[String],
    ) -> Self {
        let grid = instruments.properties(symbol).copied().unwrap_or_else(fallback_properties);
        // The DECLARED LEGS' own properties, out of the SAME already-loaded `instruments` — no
        // second round trip. Deliberately NOT falling back to `fallback_properties` the way the
        // mounted symbol does: that fallback is a venue-wide guess that happens to be right for the
        // mounted market, and stamping it onto a leg would declare a grid nobody resolved. An
        // unknown leg gets no row, and `vike-mount`'s `symbol_grid::warn_ungridded_legs` reports it
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
        MountFacts { grid, margin_mode: ruling_margin_mode, product, declared_leg_properties }
    }

    /// The live mount these facts describe, around its two clients — the ONE place a fact becomes a
    /// [`LiveMount`] field.
    fn into_live_mount(
        self,
        client: Box<dyn vike_exec::ExecutionClient + Send>,
        recon: Box<dyn vike_exec::recon::ReconClient>,
    ) -> LiveMount {
        LiveMount {
            client,
            recon,
            grid: self.grid,
            margin_mode: self.margin_mode,
            declared_leg_properties: self.declared_leg_properties,
        }
    }
}

/// Build Hyperliquid's live exec client (+ its reconcile handle) from the bespoke
/// `HYPERLIQUID_DEMO_*`/`HYPERLIQUID_LIVE_*` key shape (a secp256k1 private key + an optional master
/// address — NOT the standard API_KEY/SECRET, so it is loaded here rather than via
/// `vike_bridge_core::credentials::load_credentials_from`). [`LiveMountAttempt::live`] is `None`
/// when the key is absent (the live gate) or any startup step fails (bad key / meta fetch) → the
/// caller falls back to paper. On success [`LiveMount`] carries the fetched grid and margin mode.
///
/// `env` is the network tier [`HyperliquidVenueMount`] resolved from the ceiling
/// ([`config::Env::for_ceiling`], decision 0095); this function reads no environment and is handed
/// no ceiling.
///
/// The `HyperliquidReconClient` is built HERE rather than through a generic recon factory because
/// HL's keyless-`/info`-against-master report reads need the bespoke [`HyperliquidTransport`]/master
/// this function already builds, not a standard `Credentials`-shaped factory. It reads on the SAME
/// network the exec trades so recon sees the account the orders actually land in; its `Product`
/// (perp vs spot balance endpoint) follows the mounted symbol's.
///
/// `market_slippage` is the DEPLOYMENT's requested emulated-market aggression band
/// (`policy.toml`'s `market_slippage`, projected by the binary and threaded through unchanged).
/// Hyperliquid has no native market order: a market intent is priced as an `Ioc` limit at
/// `mid * (1 ± band)` and a tripped stop-MARKET at `trigger * (1 ± band)`, so this band is the WORST
/// price such an order is allowed to reach. It is resolved ONCE here, through
/// [`crate::exec::market_slippage_for`] — which returns the adapter's own historical literal on
/// `None` (byte-identical to every mount before this parameter existed) and CLAMPS an out-of-range
/// value into the workspace bounds.
///
/// `declared_legs`' own grids come out of the SAME already-loaded instrument fetch — see
/// [`LiveMount::declared_leg_properties`].
///
/// `halt_path` is the operator HALT sentinel the exec client watches
/// (`MountInputs::process.halt_path`, the one file `vike-mount` resolved for the whole process),
/// handed to the client with `HyperliquidExecutionClient::with_halt_path`: this function resolves
/// no process-global path (decision 0099).
#[allow(clippy::too_many_arguments)]
pub(crate) fn live_mount_for_account(
    symbol: &str,
    declared_legs: &[String],
    env: config::Env,
    vars: &HashMap<String, String>,
    label: &AccountLabel,
    live_events: &vike_exec::EventSender,
    recon_trigger: Option<std::sync::mpsc::Sender<()>>,
    market_slippage: Option<f64>,
    halt_path: std::path::PathBuf,
) -> LiveMountAttempt {
    let mainnet = env == config::Env::Live;
    // ⚠ `load_for_account`, not `load` — this is the ONE line that keeps a labelled account from
    // signing with the DEFAULT account's private key. For `AccountLabel::Default` the two are the
    // same call over the same key names.
    let Some(creds) = config::load_for_account(env, label, vars) else {
        return LiveMountAttempt::none(); // absent key = the live gate → paper
    };
    let signer = match crate::signing::Signer::from_private_key(&creds.private_key, creds.network) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("hyperliquid: invalid private key ({e}); staying paper");
            return LiveMountAttempt::none();
        }
    };
    // ONE per-IP REST weight window for every REST path this mount opens — the instruments load and
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
    let instruments =
        match crate::instruments::HyperliquidInstruments::load_from_vars(&transport, None, vars) {
            Ok(i) => i,
            Err(e) => {
                tracing::warn!("hyperliquid: meta/spotMeta load failed ({e}); staying paper");
                // The identity probe above already ran and may already be worth recording — that does
                // not depend on whether the mount goes on to succeed.
                return LiveMountAttempt { live: None, master: Some(master_outcome) };
            }
        };
    // Every venue fact this mount carries, off the SAME `instruments` — see [`MountFacts`].
    let facts = MountFacts::of(&instruments, symbol, declared_legs);
    // Reconcile handle: reuse the SAME transport (moved in — the instruments load above only
    // borrowed it) + master this function already built; keyless `/info` reads against the master on
    // `creds.network`, its balance endpoint following the mounted symbol's product.
    let recon: Box<dyn vike_exec::recon::ReconClient> =
        crate::recon_client(transport, master.clone(), facts.product);
    // Two lines, not one with substituted words: `⚠ REAL-MONEY: ` marks the live tier and nothing else
    // (`crates/vike-ops/tests/wiring/live_mount_line_gate.rs`).
    if mainnet {
        tracing::warn!(
            venue = crate::consts::VENUE,
            account = %label,
            tier = Tier::Live.as_str(),
            "⚠ REAL-MONEY: hyperliquid: LIVE credentials present → LIVE exec client (real MAINNET orders)"
        );
    } else {
        tracing::warn!(
            venue = crate::consts::VENUE,
            account = %label,
            tier = Tier::Demo.as_str(),
            "hyperliquid: DEMO credentials present → LIVE exec client (real testnet orders)"
        );
    }
    // Builder Codes (task 7, unified-venue-attribution): resolved ONCE from the workspace `.env`.
    // `attribution_code_from` validates the address against `hyperliquid`'s
    // `AttributionMechanic::SignedBuilder`; absent/invalid degrades to `None` (byte-identical — no
    // `builder` object on the wire at all, matching every mount before this field existed). The
    // fee defaults to 0 (attribution-only, no on-chain `approveBuilderFee` needed — see
    // `crate::builder_fee`'s module doc) unless `HYPERLIQUID_BUILDER_FEE_TENTHS_BP` is set AND the
    // operator has separately run the one-time approval from the master wallet.
    let builder =
        vike_bridge_core::credentials::attribution_code_from(vars, "hyperliquid").map(|address| {
            let fee_tenths_bp = vars
                .get("HYPERLIQUID_BUILDER_FEE_TENTHS_BP")
                .and_then(|s| s.trim().parse::<u32>().ok())
                .unwrap_or(0);
            HlBuilderFee { address: address.trim().to_ascii_lowercase(), fee_tenths_bp }
        });
    // The emulated-market band (Phase 6c), resolved ONCE here and carried into the exec thread.
    // `market_slippage_for(None)` IS `DEFAULT_MARKET_SLIPPAGE`, so an unconfigured mount is
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
    LiveMountAttempt {
        live: Some(facts.into_live_mount(client, recon)),
        master: Some(master_outcome),
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
/// identity probe failing should stop a mount, and the answer is always no — see [`live_mount_for_account`]'s
/// own note. Folding the decision in here keeps it from being re-made differently at a second call site.
fn resolve_master(
    transport: &HyperliquidTransport,
    signer_address: &str,
    label: &AccountLabel,
) -> MasterOutcome {
    let answer = crate::user_role::user_role(transport, signer_address);
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

/// **What the venue said about an address the OPERATOR wrote** — the verdict behind the audit of a
/// configured `HYPERLIQUID_{TIER}_ACCOUNT_ADDRESS`.
///
/// A separate type from `vike_model::accounts::account_confirmation::Verdict` on purpose, because it compares
/// a different PAIR. That one compares the venue's answer against the `account` TABLE (a
/// `vike-mount`-only comparison); this one compares it against the CREDENTIAL STORE's setting — the
/// value that decides which address this mount actually reads and trades. They can disagree
/// independently and mean different things.
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
/// `vike-mount`'s `book_identity::normalize_book`, kept as its own tiny copy here rather than a
/// cross-crate call — this bridge does not otherwise depend on `vike-mount`'s book-identity table,
/// and a bridge may not gain that dependency just to reuse a one-line string helper (the layer rule
/// runs the other way).
fn normalize_address(raw: &str) -> String {
    raw.trim().to_ascii_lowercase()
}

/// **Audit an address the operator WROTE, against the venue's own answer** — and return the address
/// this mount will use, which is ALWAYS the configured one.
///
/// # ⚠ It audits; it does not re-route
///
/// The returned address is `configured`, unchanged, on every arm including
/// [`ConfiguredAudit::Disagrees`]. That is deliberate and it is the same posture
/// `vike_model::accounts::account_confirmation::verdict` takes for its own disagreement:
///
/// * **it does not switch to the venue's answer.** That would move which account this process reads
///   and trades, silently, on a boot the operator did not ask for — a routing change nobody
///   requested is worse than a wrong value they can be told about;
/// * **it does not refuse the mount.** The session authenticated. Taking the venue away over a
///   bookkeeping disagreement degrades a capability that was working
///   (`docs/decisions/0013-degrade-vs-refuse.md`), and the credential schema's §8 rules this exact
///   case: *a violation discovered AT A HANDSHAKE is REPORTED, not thrown*;
/// * **it parks NOTHING on a disagreement** — `confirmed: false` — for the same reason `resolve_master`
///   does not record an unanswered probe: a disagreement is a finding about the CONFIGURED value, not
///   a confirmation of what the mount is actually using.
///
/// # What it costs, and why that was a decision rather than an omission
///
/// One `userRole` request per hyperliquid mount that has a configured address — weight 60 of a
/// shared 1200-per-minute per-IP budget, once, at startup. Until 2026-09-20 this path made NO
/// request at all and the account was never verified; `crate::user_role`'s module doc carries what
/// the weight means and why the probe may never sit in a loop.
///
/// What it buys is the one comparison nothing else in the tree performs. Every other check reads the
/// configured address and trusts it. If it is stale or mistyped, this mount reads the WRONG
/// account — and Hyperliquid answers a wrong address with `accountValue: "0.0"` rather than an
/// error, so the result is a book that looks flat and healthy while reconcile finds nothing to
/// reconcile. That is precisely the failure `vike_model::accounts::account_confirmation` exists to remove, on
/// the one input that had no check at all.
fn audit_configured_master(
    transport: &HyperliquidTransport,
    signer_address: &str,
    label: &AccountLabel,
    configured: String,
) -> MasterOutcome {
    let answer = crate::user_role::user_role(transport, signer_address);
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
                 `HYPERLIQUID_{{TIER}}_ACCOUNT_ADDRESS` for this account says `{configured}`, and \
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
        // ⚠ The probe failed to ASK. Never a reason to refuse the mount: that would trade the
        // venue's REST availability for this box's ability to start.
        Err(e) => fallback(format!(
            "could not ask the venue which account this key trades for ({e}), so the key's own \
             address is assumed to BE the account — true for a master key, WRONG for an API wallet, \
             where it reads an empty book."
        )),
    }
}

/// The contract's tier for a network tier.
fn tier_of(env: config::Env) -> Tier {
    match env {
        config::Env::Live => Tier::Live,
        config::Env::Demo => Tier::Demo,
    }
}

/// `{"specialStatuses":null,"time":<epoch ms>}` — the `exchangeStatus` body. Split from the read
/// so the field is gated by a fixture test rather than by a live call.
fn parse_server_time(body: &serde_json::Value) -> Result<i64, String> {
    body.get("time").and_then(serde_json::Value::as_i64).ok_or_else(|| missing_time_field("time"))
}

/// A mount attempt as the contract's outcome — PURE, so every field's fold is testable with no
/// key and no network (`mount_contract_tests.rs`).
///
/// ⚠ The identity is carried on EVERY outcome the probe confirmed, the PAPER one included: the
/// `userRole` probe runs before `meta`, so a `meta` failure can follow a confirmed answer, and
/// `vike-mount`'s old arm recorded it anyway. Its tier is the NETWORK the handshake ran on, never
/// the ceiling in scope.
fn outcome_from_attempt(attempt: LiveMountAttempt, env: config::Env) -> MountOutcome {
    let tier = tier_of(env);
    let identity = attempt.master.filter(|m| m.confirmed).map(|m| IdentityReport {
        book: m.address,
        evidence: "`userRole`",
        tier,
    });
    let Some(live) = attempt.live else {
        return MountOutcome { exec: ExecOutcome::Paper, recon: None, identity };
    };
    MountOutcome {
        exec: ExecOutcome::Live(LiveExec {
            client: live.client,
            bound_tier: tier,
            grid: Some(live.grid),
            contract_size: None,
            margin_mode: Some(live.margin_mode),
            leg_grids: live.declared_leg_properties.into_iter().collect(),
        }),
        recon: Some(live.recon),
        identity,
    }
}

/// hyperliquid's mount. `vike_tradehub::registry::REGISTRY` holds `&HyperliquidVenueMount`.
pub struct HyperliquidVenueMount;

impl VenueMount for HyperliquidVenueMount {
    fn venue(&self) -> &'static str {
        crate::consts::VENUE
    }

    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration {
            addresses_accounts: true,
            process_exclusive: None,
            // HL's exec pump pokes the reconcile driver on every WS reconnect.
            takes_recon_trigger: true,
            // `live_mount_for_account` loads `HyperliquidInstruments` for the WHOLE venue before it
            // resolves the mounted symbol's properties, so every other asset is already in hand.
            grid_source: DeclaredGridSource::InHand,
            // `crate::config`'s `load_for_account`: `_ACCOUNT_ADDRESS` is the MASTER address when
            // the key is an agent wallet, and absent means "the key IS the account" — derivable
            // offline from the EVM key. Sub-accounts have their own addresses, so two of them are
            // two books.
            book_identity: BookIdentity::Named {
                prefix: "HYPERLIQUID",
                demo_tiers: &["DEMO"],
                live_tiers: &["LIVE"],
                name_suffixes: &["ACCOUNT_ADDRESS"],
                evm_key_suffixes: &["PRIVATE_KEY"],
            },
            clock: ClockDecl::Wired {
                endpoint: "POST /info {\"type\":\"exchangeStatus\"} (public)",
                auth: ClockAuth::Public,
                risk: ClockRisk::NonceWindow,
            },
        }
    }

    /// The key for the ceiling's network arms that network — INTENT, not outcome: a present key
    /// that will not parse still arms, so the budget refusal fires before any session, and
    /// [`Self::mount`] declines it into paper. `held_below_live` is always `None`: testnet is
    /// reached only under a ceiling below `live`.
    ///
    /// No key for that network is paper, and under a `live` ceiling the cause is the LIVE key's
    /// absence — whatever the DEMO tier holds, because mainnet is never signed with a testnet key.
    fn resolve(&self, inputs: &MountInputs<'_>) -> Resolution {
        let env = config::Env::for_ceiling(inputs.live_permitted);
        if config::load_for_account(env, inputs.account, inputs.secrets).is_some() {
            Resolution::Armed { tier: tier_of(env), held_below_live: None }
        } else if inputs.live_permitted {
            Resolution::Paper(PaperCause::LiveCredentialsAbsent)
        } else {
            Resolution::Paper(PaperCause::NoCredentials)
        }
    }

    fn mount(&self, req: MountRequest<'_>) -> MountOutcome {
        let env = config::Env::for_ceiling(req.inputs.live_permitted);
        // THE POLICY CONSUMER: HL is the one roster venue with no native market order, so
        // `req.market_slippage` binds here and nowhere else (`crate::exec`'s
        // `market_slippage_for` resolves `None` to the adapter's historical literal).
        let attempt = live_mount_for_account(
            req.symbol,
            req.declared_legs,
            env,
            req.inputs.secrets,
            req.inputs.account,
            req.events,
            req.recon_trigger,
            req.market_slippage,
            req.inputs.process.halt_path.clone(),
        );
        outcome_from_attempt(attempt, env)
    }

    /// `POST /info {"type":"exchangeStatus"}` — keyless, over the crate's OWN transport on an
    /// agent bounded by `timeout`, on the network the ceiling selects. The two networks' clocks
    /// genuinely differ, so this must not be simplified to "always mainnet".
    fn server_time_ms(&self, inputs: &MountInputs<'_>, timeout: Duration) -> Result<i64, String> {
        let body = HyperliquidTransport::with_agent(
            config::Env::for_ceiling(inputs.live_permitted).network(),
            vike_bridge_core::http::blocking_agent_with_timeout(timeout),
        )
        .info(&serde_json::json!({ "type": "exchangeStatus" }))
        .map_err(|e| e.to_string())?;
        parse_server_time(&body)
    }
}

#[path = "mount_tests.rs"]
#[cfg(test)]
mod mount_tests;

#[path = "master_resolution_tests.rs"]
#[cfg(test)]
mod master_resolution_tests;

#[path = "mount_contract_tests.rs"]
#[cfg(test)]
mod mount_contract_tests;
