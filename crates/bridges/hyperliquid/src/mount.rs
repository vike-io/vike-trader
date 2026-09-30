//! Hyperliquid's live-mount seam: signer + transport wiring, the venue's own `userRole` identity
//! probe, and the exec+recon client construction `vike-mount`'s `("hyperliquid", _)` arm calls.
//!
//! Moved down from `crates/vike-mount/src/hyperliquid.rs` on decision 0088's B3
//! (`docs/decisions/0088-mount-sheds-venue-facts-to-their-bridges.md`). That file was never a
//! considered placement to begin with — it was extracted VERBATIM from `vike-app/src/main.rs` when
//! the GUI's local composition root was cut, and rode along with whichever crate the desktop rename
//! happened to leave it in. Everything below needs a resolved network tier, a credential map and an
//! account label — exactly what every other seam in this bridge already takes — and none of it needs
//! `vike-mount`'s own `MountPolicy`, `VenueMode` or `AccountDirectory` types.
//!
//! # What deliberately did NOT move here
//!
//! * **The network read.** The network is the arming ceiling (`Env::for_ceiling`, decision 0095);
//!   decision 0088's residual B2 is resolved by deleting the switch. [`live_mount_for_account`]
//!   therefore takes the ALREADY-RESOLVED [`crate::config::Env`] as a plain parameter; it reads no
//!   environment and is handed no ceiling to fold.
//! * **Recording what the venue said about this account.** The `userRole` probe still runs here — it
//!   needs the `transport`/`signer` this module already builds, so splitting the NETWORK call out
//!   would mean building a second transport just to ask it, which would silently double this venue's
//!   per-IP REST spend (see [`ip_gate_and_transport`]'s own doc). But PARKING the answer for
//!   `vike-cli secrets confirm` needs `vike-mount`'s `AccountDirectory` and `VenueMode`, and a bridge
//!   may hold neither (0088's hard rule: an arming ceiling crosses into a bridge only as a `bool`,
//!   never as `MountPolicy`/`VenueMode`). So the probe answers with a [`MasterOutcome`] — the address
//!   and whether the venue CONFIRMED it — and `vike-mount`'s composition root performs the actual
//!   store write once it has that answer back.
//! * **Folding declared-leg grids into `RiskLimits::grid_by_symbol`.** `vike-mount`'s
//!   `declared_symbol_grids` is `pub(crate)` to that crate on purpose (it is the one place that
//!   understands the mount's grid-fallback rules), so this module hands back the raw per-leg
//!   `SymbolProperties` it already fetched ([`LiveMount::declared_leg_properties`]) and the caller
//!   folds them exactly as it always did.

use std::collections::HashMap;

use indexmap::IndexMap;
use vike_bridge_core::ratelimit::RateGate;
use vike_model::account_keys::AccountLabel;

use crate::config;
use crate::signing::action::HlBuilderFee;
use crate::symbology::Symbology;
use crate::transport::HyperliquidTransport;
use crate::user_role::UserRole;

/// FALLBACK Hyperliquid BTC-perp grid (szDecimals-5 → tick 0.1 / step 1e-5, $10 min notional),
/// used ONLY if the adapter's startup `meta` fetch fails. The live client fetches the real grid.
///
/// Its only caller is [`live_mount_for_account`], which is the reason it followed that function
/// down from `vike-mount` rather than joining `crates/vike-mount/src/fallback.rs`'s general table —
/// that file's own module doc already carved this one out ("Hyperliquid's fallback grid lives next
/// to its live-client helper… not here"), and the caller it sits beside is now this one.
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
        // nothing — see `vike-mount`'s `fallback.rs` for the same reasoning.
        ..Default::default()
    }
}

/// The mounted symbol's RULING margin mode, read off the venue's own `meta` — the mode a position
/// on this asset OPENS IN when the order names none.
///
/// `pub` (rather than the `pub(crate)` most of this module's pieces stay) because `vike-mount`'s own
/// wiring test drives this function end-to-end into its `margin_mode_grid`/`Account` fold — proving
/// the two halves stay CONNECTED is exactly the point of that test, and it can only do that by
/// calling the real function across the crate boundary.
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
/// (`vike_model::venue_rate_limits::HYPERLIQUID`'s `rest_ip_weight`), so the number of `RateGate`s
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
/// The caller is `vike-mount`'s `("hyperliquid", _)` arm, which folds a `confirmed` outcome into
/// `crate::book_identity::record_confirmation` — a write this bridge cannot perform itself, since it
/// needs `vike-mount`'s own `AccountDirectory` and `VenueMode`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MasterOutcome {
    pub address: String,
    pub confirmed: bool,
}

/// A live Hyperliquid exec + recon mount, and what the caller needs to finish wiring it in.
pub struct LiveMount {
    pub client: Box<dyn vike_exec::ExecutionClient + Send>,
    pub recon: Box<dyn vike_exec::recon::ReconClient>,
    /// Per-DECLARED-LEG `SymbolProperties`, resolved from the SAME instrument fetch this mount
    /// already performed — no second round trip. A leg the venue's `meta`/`spotMeta` does not list
    /// is simply absent. `vike-mount`'s `("hyperliquid", _)` arm folds these into
    /// `vike_exec::RiskLimits::grid_by_symbol` through its own `crate::symbol_grid::
    /// declared_symbol_grids` — a `pub(crate)` helper this bridge cannot call directly.
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
pub struct LiveMountAttempt {
    pub live: Option<LiveMount>,
    pub master: Option<MasterOutcome>,
}

impl LiveMountAttempt {
    fn none() -> Self {
        Self { live: None, master: None }
    }
}

/// Build Hyperliquid's live exec client (+ its reconcile handle) from the bespoke
/// `HYPERLIQUID_DEMO_*`/`HYPERLIQUID_LIVE_*` key shape (a secp256k1 private key + an optional master
/// address — NOT the standard API_KEY/SECRET, so it is loaded here rather than via
/// `vike_bridge_core::credentials::load_credentials_from`). [`LiveMountAttempt::live`] is `None`
/// when the key is absent (the live gate) or any startup step fails (bad key / meta fetch) → the
/// caller falls back to paper. On success it sets `limits`/`margin_mode_out` from the fetched grid.
///
/// `env` is `Env::for_ceiling(live_permitted)` — the ALREADY-RESOLVED network tier (decision 0095:
/// the arming ceiling alone chooses it); this function reads no environment and is handed no
/// ceiling.
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
/// `margin_mode_out` is the SECOND out-parameter, written the same way and for the same reason as
/// `limits`: it is a fact only the venue's `meta` fetch knows, and the `Account` the caller builds
/// afterward needs it. It receives the mounted symbol's RULING margin mode — see [`margin_mode`] —
/// so a position opened from flat on one of this venue's isolated-only assets books `Isolated`
/// rather than `Cross`. It is left untouched on every path that does not reach a live `meta`.
///
/// `declared_legs`' own grids come out of the SAME already-loaded instrument fetch — see
/// [`LiveMount::declared_leg_properties`].
#[allow(clippy::too_many_arguments)]
pub fn live_mount_for_account(
    symbol: &str,
    declared_legs: &[String],
    env: config::Env,
    vars: &HashMap<String, String>,
    label: &AccountLabel,
    live_events: &vike_exec::EventSender,
    limits: &mut vike_exec::RiskLimits,
    margin_mode_out: &mut vike_model::MarginMode,
    recon_trigger: Option<std::sync::mpsc::Sender<()>>,
    market_slippage: Option<f64>,
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
    *limits = instruments
        .properties(symbol)
        .map(vike_exec::RiskLimits::from_properties)
        .unwrap_or_else(|| vike_exec::RiskLimits::from_properties(&fallback_properties()));
    // The DECLARED LEGS' own properties, out of the SAME already-loaded `instruments` — no second
    // round trip. Deliberately NOT falling back to `fallback_properties` the way the mounted symbol
    // does: that fallback is a venue-wide guess that happens to be right for the mounted market, and
    // stamping it onto a leg would declare a grid nobody resolved. An unknown leg gets no row, and
    // `vike-mount`'s `symbol_grid::warn_ungridded_legs` reports it once the caller folds this map.
    let mut declared_leg_properties = IndexMap::new();
    for leg in declared_legs {
        if let Some(p) = instruments.properties(leg) {
            declared_leg_properties.insert(leg.clone(), *p);
        }
    }
    // Reconcile handle: reuse the SAME transport (moved in — the instruments load above only
    // borrowed it) + master this function already built; keyless `/info` reads against the master on
    // `creds.network`. The balance endpoint (perp `clearinghouseState` vs spot
    // `spotClearinghouseState`) follows the mounted symbol's product; default perp.
    let product = instruments
        .symbology()
        .by_symbol(symbol)
        .map(|i| i.product)
        .unwrap_or(config::Product::Perp);
    // The OTHER mount-time per-asset fact, from the same fetched `Symbology`: the mode a position
    // on this asset opens in (see [`margin_mode`]). Written HERE and nowhere else — a paper fallback
    // never reaches this line, so it keeps the caller's `Cross`.
    *margin_mode_out = margin_mode(instruments.symbology(), symbol);
    let recon: Box<dyn vike_exec::recon::ReconClient> =
        crate::recon_client(transport, master.clone(), product);
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
    let client: Box<dyn vike_exec::ExecutionClient + Send> =
        Box::new(crate::HyperliquidExecutionClient::spawn_with_market_slippage(
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
    LiveMountAttempt {
        live: Some(LiveMount { client, recon, declared_leg_properties }),
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
/// A separate type from `vike_model::account_confirmation::Verdict` on purpose, because it compares
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
/// `vike_model::account_confirmation::verdict` takes for its own disagreement:
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
/// reconcile. That is precisely the failure `vike_model::account_confirmation` exists to remove, on
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

#[path = "mount_tests.rs"]
#[cfg(test)]
mod mount_tests;

#[path = "master_resolution_tests.rs"]
#[cfg(test)]
mod master_resolution_tests;
