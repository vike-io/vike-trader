//! The Polymarket VENUE ADAPTER — [`VenueAdapterPolymarket`], this venue's implementation of the
//! `vike_bridge_core::venue_adapter::VenueAdapter` contract
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md), and the live-mount
//! factory it calls, so no signer / derivation / address / websocket detail lives in the
//! composition root (the same discipline
//! [`crate::reconcile::reconcile_client_for_account`] follows for the recon half).
//! `vike-tradehub-core`'s `("polymarket", _)` arm, its arming-probe row and its clock, book-identity and
//! grid-source rows moved here, behaviour unchanged. The egress the factory dials through is the
//! process-wide `declare_egress` cell the composition root fills from the settings database —
//! the one named exception to "a bridge reads no process global"; the start reads no environment
//! and no store.
//!
//! [`live_runtime_for_account`] returns the exec client AND the recon client **built over ONE shared
//! [`OrderVenueIdTracker`] and ONE L2 handshake**. That sharing is the point: the CLOB assigns order
//! ids and never echoes a client id, so a reconcile report can only name a local order client id if it reads
//! the very map the exec thread writes. Before this existed, `make_engine` built the recon client
//! with a FRESH empty registry and every order report came back `order_client_id: None`.
//!
//! ## What is gated, and why there are two gates
//! - `flags.poly_exec` ([`polymarket_execution_enabled`]) — the exec gate. Polymarket has **no testnet**: every
//!   order this starts is real money on Polygon mainnet, so the venue is opt-in twice over (creds
//!   AND this flag) rather than the usual once. Off ⇒ [`VenueAdapterPolymarket`] builds nothing here,
//!   makes no network call, and the account is NOT RUNNING.
//! - `flags.poly_reconcile` ([`crate::reconcile::polymarket_reconcile_enabled`]) — gates the
//!   reconcile half of an account that runs. It starts nothing without `flags.poly_exec`: exec-only,
//!   both, or neither.
//!
//! A THIRD flag, `venue.polymarket.presubmit_register` ([`presubmit_register_enabled`]), is NOT a
//! start gate but a within-exec behavior toggle threaded onto
//! [`PolymarketLiveConfig::presubmit_register`]: ON,
//! the exec thread pre-registers `order_client_id`↔`derive_order_id(&order)` just before each submit to close
//! the ack-race (a user-WS fill can beat the HTTP ack). Default OFF ⇒ byte-identical to today.
//!
//! None of them is read from the environment (decision 0095). The two flags reach the start as bools
//! the composition root resolved (`AdapterInputs::flags`, decision 0095's amendment of 2026-11-22);
//! `presubmit_register`, `exec_markets` and
//! the rate gate are `venue.polymarket.*` rows read from the venue's settings
//! (`AdapterInputs::settings`) — the store folded them into the credential map under legacy names
//! until decision 0095's Task 7 retired that fold.
//!
//! ## The region pre-flight (why an exec start can now REFUSE)
//!
//! Polymarket restricts ORDER PLACEMENT by region and nothing else: from a restricted egress the
//! market feeds stream, `/auth/derive-api-key` succeeds and every authenticated READ — balance,
//! positions, orders, fills — comes back green, while a submit returns a 403 saying trading is
//! restricted in your region. A wrong-region session therefore looked perfectly healthy right up
//! to the moment it tried to trade, which on an exec path is the worst possible time to find out.
//!
//! [`live_runtime_for_account`] now asks the venue first, through its geoblock row
//! (`crate::config`'s `geoblock_base`, `/api/geoblock` appended by [`crate::transport::observe_geoblock`]) and
//! the SAME resolved proxy the orders will use, and REFUSES the live exec start when the answer is
//! `blocked` — naming the country and region, and saying that reads are unaffected. It refuses on
//! a POSITIVE answer only: an unreachable probe or an unreadable body warns and proceeds
//! ([`geoblock_action`]). The check is EXEC-ONLY on purpose — reads are legal from a restricted
//! region, so gating the feeds on it would break the one setup that is legitimately region-blind.
//!
//! `POLY_GEOBLOCK_OVERRIDE` = `1` ([`geoblock_override_enabled`]) skips it — a credential-store row
//! (the exec and reconcile gates are bools on `AdapterInputs::flags`), and its doc carries the argument.
//!
//! The **dynamic tick-size regime** ([`crate::instrument::tick_regime`]) is deliberately NOT a flag at all: this
//! factory always builds one and threads it into the exec thread, because it exists to repair a live
//! failure the venue reports only as an endless stream of rejects, and an operator cannot know to
//! turn it on before hitting that. Starting it is safe to do unconditionally because it starts EMPTY
//! and an unresolved token prices verbatim, so no price moves until an off-grid reject has actually
//! taught it that token's grid. It rides `flags.poly_exec` — an exec-less start builds nothing here.

use std::collections::HashMap;

use vike_bridge_core::transport::ErrorKind;
use vike_bridge_core::venue_adapter::{
    AdapterFlags, AdapterInputs, AdapterOutcome, AdapterRequest, ArmingNotRunningCause,
    ArmingResolution, BookIdentityRule, ClockDeclaration, VenueAdapter, VenueAdapterDeclaration,
    VenueAdapterExecutionOutcome, VenueAdapterExecutionRunning,
};
use vike_catalog::is_outcome_token;
use vike_execution::recon::ReconcileClient;
use vike_execution::{EventSender, ExecutionClient};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{AccountTier, GridSource};
use vike_secrets::account_credentials::AccountCredentials;

use crate::config::{PolymarketConfig, row_or_not_running};
use crate::execution::registry::OrderVenueIdTracker;
use crate::execution::{
    PolymarketLiveConfig, UserChannelConfig, decode_builder_bytes32, spawn_live,
};
use crate::instrument::tick_regime::TickRegime;
use crate::neg_risk_lookup::NegRiskSource;
use crate::reconcile::{
    polymarket_reconcile_enabled, recon_client, reconcile_client_for_account,
    signature_type_for_account,
};
use crate::refusals::PolymarketStartRefusal;
use crate::transport::{GeoblockVerdict, check_order_placement_geo};
use vike_secrets::venue_setting::{SettingTier, VenueSettings};
use vike_venues::venue_fields::{EXEC_MARKETS, PRESUBMIT_REGISTER, VenueFieldGrammar};
use vike_venues::venue_urls::VenueUrls;

/// The credential-store name of the geoblock pre-flight's escape hatch (see
/// [`geoblock_override_enabled`]).
pub const POLYMARKET_GEOBLOCK_OVERRIDE_ENV: &str = "POLY_GEOBLOCK_OVERRIDE";

/// The exec opt-in: `flags.poly_exec`, as the composition root resolved it and hands the start
/// (`AdapterInputs::flags`; the bridge names no settings key, decision 0095's amendment of 2026-11-22).
/// Default OFF.
///
/// This is a SECOND gate on top of absent-credentials-is-the-live-gate, and it exists because
/// Polymarket is the one started venue with no demo tier at all: on every other venue a mis-set
/// credential starts a *demo* client, here it starts a real one. `flags.poly_exec` off means an operator
/// who merely has keys in their `.env` — which the reconcile half, the data feeds and the redeem
/// poller all legitimately want — cannot accidentally arm order placement.
pub fn polymarket_execution_enabled(flags: &AdapterFlags) -> bool {
    flags.poly_exec
}

/// The pre-submit-registration opt-in: `venue.polymarket.presubmit_register` read typed
/// (`VenueSettings::get_bool`, its declared `ExactOne` grammar: the trimmed exact `1`, so an annotated
/// `1 # …` is not on), from the venue's settings (decision 0095). Default OFF.
///
/// ON ⇒ the exec thread pre-registers `order_client_id`↔`crate::execution::order::derive_order_id(&order)` in the shared
/// registry just BEFORE each network submit, closing the ack-race in which a user-WS fill/cancel
/// beats the HTTP acceptance and lands keyed by an order id the registry does not yet hold (the
/// `order_client_id: None` path). Safe because that derived id is the CLOB's own `orderID` — the
/// venue keys orders by the EIP-712 order hash — and the later real `OrderAccepted` re-registers the
/// SAME `(order client id, id, side)` as an idempotent no-op (see [`OrderVenueIdTracker::on_accept`]).
///
/// Kept a SEPARATE gate from `POLY_EXEC` (rather than always-on with exec) as verify-before-enable
/// discipline: the `derive_order_id == orderID` premise is LIVE-VERIFIED on a real demo order
/// (`crate::execution::tests::live_place_and_cancel`), but booking a pre-registered id into the money
/// path is opt-in until that smoke is re-run against the deployment. Unset (the default) ⇒ nothing
/// is registered pre-submit — byte-identical to before this existed.
///
/// ⚠ **This flag is no longer the only thing standing between a raced fill and a silent loss, and
/// must not be read as such.** It ships OFF, and while it was the sole mitigation an executed fill
/// that beat the ack was simply discarded at `crate::execution::user_data::decode`'s re-key sites — position and PnL
/// silently wrong. The unconditional staging in [`crate::execution::pending_events`] now covers that race in
/// every build, flag or no flag. What the flag still buys is AVOIDING the stage entirely (the id is
/// already there when the frame lands), which is cheaper and needs no TTL to be right — but it is an
/// optimisation over the default now, not the safety net.
pub fn presubmit_register_enabled(settings: &VenueSettings) -> bool {
    settings.get_bool(SettingTier::Any, &PRESUBMIT_REGISTER) == Some(true)
}

/// The CLOB **condition ids** the user-channel subscribe frame names, from the
/// `venue.polymarket.exec_markets` row in the venue's settings (comma- and/or
/// whitespace-separated).
///
/// ⚠ These are market `condition_id`s, NOT outcome `token_id`s — the two look alike (both long hex/
/// decimal strings) and the user channel silently streams nothing if you hand it token ids. The
/// `markets` field of the subscribe frame is the same one
/// [`user_subscribe_message`](crate::execution::user_data::decode::user_subscribe_message) takes and the
/// `polymarket_user_smoke` passes a `condition_id` to.
///
/// An EMPTY list (the default) subscribes account-wide — the CLOB user channel treats an empty
/// `markets` array as "every market this key trades", which is what a mount wants when the strategy
/// picks its markets at runtime. That behavior is asserted live by
/// `polymarket_execution_mount_smoke::empty_markets_is_account_wide`, because it is a server-side convention
/// with no contractual documentation; if a future deployment changes it, set
/// `venue.polymarket.exec_markets` explicitly and the pump narrows to exactly those markets.
/// **Either way the A3 resync is the backstop**: it replays `/data/trades` + `/data/orders`, which
/// are account-wide regardless of the subscribe list, after every reconnect.
pub fn polymarket_execution_markets(settings: &VenueSettings) -> Vec<String> {
    let raw = settings.get(SettingTier::Any, &EXEC_MARKETS).unwrap_or_default().to_string();
    raw.split(|c: char| c == ',' || c.is_whitespace())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        // `#` starts a trailing `.env` comment; everything from it on is annotation, not a market.
        .take_while(|s| !s.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// The geoblock pre-flight's escape hatch, read from the caller-supplied map with the one exact-`1`
/// switch rule (`vike_venues::venue_fields::VenueFieldGrammar::ExactOne`: the trimmed exact `1`, so an
/// annotated `1 # …` does NOT skip the pre-flight). Default OFF ⇒ the pre-flight runs.
///
/// Map-only since decision 0095 (the two flags are bools on `AdapterInputs::flags`). It ARMS nothing on its own:
/// `flags.poly_exec` is still required, and a `POLY_EXEC` credential row or variable refuses
/// startup — all this flag can do is decline a pre-flight, and a genuinely blocked region still
/// refuses each order at submit.
pub fn geoblock_override_enabled(vars: &HashMap<String, String>) -> bool {
    vars.get(POLYMARKET_GEOBLOCK_OVERRIDE_ENV)
        .and_then(|v| VenueFieldGrammar::ExactOne.switch_value(v))
        == Some(true)
}

/// What a live exec start does about the venue's geoblock verdict — the three outcomes, kept as
/// data so [`geoblock_action`] can be decided (and tested) without a logger or a network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeoblockAction {
    /// The venue confirmed this egress may place orders. Start, say nothing.
    Proceed,
    /// Start anyway, but say why the pre-flight proved nothing.
    Warn(PolymarketStartRefusal),
    /// REFUSE the live exec start, naming where the venue is refusing from.
    Refuse(PolymarketStartRefusal),
}

/// The PURE decision behind the pre-flight — no network, no environment, so BOTH halves of the
/// contract are unit-testable rather than asserted in prose:
///
/// - a `Blocked` verdict **REFUSES** the live exec start, naming the country and the region and
///   saying explicitly that reads are unaffected (they are: the same egress that gets a 403 at
///   submit fetches balance, positions, orders and fills perfectly well);
/// - an `Unknown` verdict — probe unreachable, body unreadable, venue having a bad day — only
///   **WARNS**. An unreachable pre-flight is evidence of nothing, and a venue outage that could
///   fail a start would be a worse defect than the silent 403 this exists to catch.
pub fn geoblock_action(verdict: GeoblockVerdict) -> GeoblockAction {
    match verdict {
        GeoblockVerdict::Allowed(_) => GeoblockAction::Proceed,
        GeoblockVerdict::Blocked(geoblock) => {
            GeoblockAction::Refuse(PolymarketStartRefusal::GeoblockBlocked { geoblock })
        }
        GeoblockVerdict::Unknown(why) => {
            GeoblockAction::Warn(PolymarketStartRefusal::GeoblockUnread { why })
        }
    }
}

/// The pre-flight as the start runs it: the operator override FIRST, then the venue's own answer.
///
/// The override short-circuits the probe rather than overruling its result, the same "inert unless
/// configured" discipline [`crate::transport::check_expected_egress`] follows: an operator who has
/// already answered this question should not pay a blocking round-trip on the start path for an
/// answer that cannot change anything. It stays LOUD either way — skipping the check is itself a
/// warning, because a skipped pre-flight and a passed one must never read the same in a log.
///
/// The probe dials the geoblock row among `urls` (`crate::config`'s `geoblock_base`); a missing row
/// REFUSES with the refusal's words, as every other missing address of a live start does.
fn geoblock_preflight(vars: &HashMap<String, String>, urls: &VenueUrls) -> GeoblockAction {
    if geoblock_override_enabled(vars) {
        return GeoblockAction::Warn(PolymarketStartRefusal::GeoblockSkipped);
    }
    match crate::config::geoblock_base(urls) {
        Ok(base) => geoblock_action(check_order_placement_geo(base)),
        Err(e) => GeoblockAction::Refuse(PolymarketStartRefusal::GeoblockRowRefused {
            why: e.to_string(),
        }),
    }
}

/// A live Polymarket runtime: the exec client (with its user-WS return lane already running inside
/// its own exec thread) plus the recon client that shares its registry.
pub struct PolymarketLiveRuntime {
    /// The client [`VenueAdapterPolymarket`] hands `vike-tradehub-core` as `VenueAdapterExecutionRunning::client`.
    pub client: Box<dyn ExecutionClient + Send>,
    /// `Some` when the caller asked for reconcile AND the L2 trio derived — the SAME creds and the
    /// SAME registry the exec client uses.
    pub reconcile: Option<Box<dyn ReconcileClient>>,
    /// The dynamic tick-size cache the exec thread prices orders on ([`crate::instrument::tick_regime`]) — the
    /// caller's clone of the SAME handle, so a quoting path can round its own quotes on whatever
    /// grid the venue has taught the exec thread (`Clone` shares one state). Consuming it is
    /// optional; the self-heal works whether or not anyone holds this end.
    pub tick_regime: TickRegime,
}

/// Build the live Polymarket runtime for ONE account: its credential FIELDS (`creds`, the account's
/// `AdapterInputs::credentials`) and the deployment's named values (`vars`: the geoblock override and
/// the builder attribution code — never a credential).
///
/// Returns `Err` — no client, so the account does NOT RUN (decision 0156) — for every one of:
/// an absent `PRIVATE_KEY` field (absent-credentials-is-the-live-gate), an unusable key, a missing
/// address row, a REFUSED region pre-flight (the venue itself says this egress may not place orders —
/// [`geoblock_action`], logged at `error!` rather than `warn!` because it is a verdict rather than an
/// absence), each `Err(None)`: refused before any venue call; or a failed L2 derivation (tunnel down /
/// geo-blocked / rejected ClobAuth signature), `Err(Some(kind))` with the handshake's `ErrorKind`, which says
/// whether a later try can pass (`AdapterOutcome::failed`). None of those is a start failure or a panic;
/// each logs its own reason. The gates themselves (`flags.poly_exec` /
/// `flags.poly_reconcile`) are the CALLER's to check — `VenueAdapterPolymarket::start` checks them so that
/// an unset flag makes no network call at all.
///
/// What it resolves that a raw `crate::execution::spawn_live` takes as given — the same
/// three things `reconcile_client_for_account` documents, because they are the same handshake:
/// 1. **The L1 signer address.** `load_polymarket_credentials_from` fills `address` from the account's
///    `ADDRESS`/`FUNDER` fields, which on a deposit-wallet (`POLY_1271`) account is the FUNDER,
///    not the key's EOA. ClobAuth must be signed by, and name, the EOA — so it is re-derived here.
/// 2. **The L2 trio**, via ONE blocking, proxy-routed `/auth/derive-api-key` round-trip at start,
///    shared by the exec client, its user-WS subscribe frame, and the recon client.
/// 3. **The funder/maker**, which stays the account's funder address (the deposit wallet the
///    orders are made for), falling back to the EOA for a bare-key account.
///
/// `seed_token` is an optional outcome `token_id` (the start's `AdapterRequest::symbol`) whose
/// NegRisk flag is pre-resolved here so the FIRST order pays no lookup latency; a failure to resolve
/// it is inert — the exec thread's lazy [`NegRiskSource::lookup`] retries per order.
///
/// `settings` is the venue's `venue_setting` rows (`AdapterInputs::settings`) — the rate gate, the
/// user-channel markets and the pre-submit registration are read from them (decision 0095).
///
/// `halt_path` is the operator HALT sentinel the exec client watches
/// (`AdapterInputs::process.halt_path`): the factory is handed it, as it is handed everything else
/// the process decided, and resolves nothing itself (decision 0099). A caller with no sentinel — a
/// smoke — passes a path it owns and never creates.
///
/// `label` names the account in the live-mount line; every credential and the signature type are
/// read out of `creds`, THIS account's own fields (`crate::config::load_polymarket_credentials_from`,
/// `crate::reconcile::signature_type_for_account`).
///
/// ⚠ **No path here reads the default account's key on behalf of a labelled one.** That is what the
/// refusal in `vike_tradehub_core::arm_addresses_accounts` used to stand in for: on a REAL-MONEY-only venue,
/// a second engine built on the first wallet's key is two engines trading one account.
///
/// ⚠ What stays DEPLOYMENT-wide, deliberately: `flags.poly_exec` (the caller's to check),
/// `venue.polymarket.exec_markets`, `venue.polymarket.presubmit_register`,
/// `venue.polymarket.rate_gate`, the geoblock pre-flight and the builder attribution code. None of
/// them names a wallet — they describe this process's egress and behaviour, so a second account
/// inherits them by being on the same box. The three `venue.polymarket.*` rows arrive as
/// `settings`, the venue's machine-scoped settings.
pub fn live_runtime_for_account(
    creds: &AccountCredentials<'_>,
    vars: &HashMap<String, String>,
    label: &AccountLabel,
    seed_token: Option<&str>,
    want_reconcile: bool,
    events: &EventSender,
    settings: &VenueSettings,
    halt_path: std::path::PathBuf,
) -> Result<PolymarketLiveRuntime, Option<ErrorKind>> {
    // Polymarket is Polygon-MAINNET-only — there is no testnet — so the live tier is the only tier.
    let loaded = crate::config::load_polymarket_credentials_from(creds).ok_or(None)?;
    let signer = match vike_bridge_core::eip712::eth_address_from_private_key(
        loaded.private_key.expose(),
    ) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(target: "vike_polymarket::adapter", error = %e, "polymarket exec unmounted: the account's PRIVATE_KEY is not a usable key");
            return Err(None);
        }
    };
    // The addresses this start dials: the venue's `venue_url` rows (`crate::config`), all at type
    // `live`. A missing row is the account NOT RUNNING with the refusal's words, before any network.
    let urls = settings.urls();
    let what = "polymarket exec";
    let clob = row_or_not_running(what, crate::config::clob_base(urls)).ok_or(None)?;
    let relayer = row_or_not_running(what, crate::config::relayer_base(urls)).ok_or(None)?;
    let gamma = row_or_not_running(what, crate::config::gamma_base(urls)).ok_or(None)?;
    let user_websocket =
        row_or_not_running(what, crate::config::user_websocket_url(urls)).ok_or(None)?;
    // The venue's OWN region pre-flight, BEFORE the L2 handshake and before a single order is
    // signed — and deliberately AFTER the credential/key gates above, so an unconfigured box still
    // makes no network call at all. Polymarket refuses ORDER PLACEMENT by region while leaving
    // market data and authenticated READS working, so without this a wrong-region session looks
    // completely healthy (it starts, streams, reconciles positions, shows balances) and learns the
    // truth only as a per-order 403 on the exec path. A block REFUSES here; an unreachable or
    // unreadable probe only warns — `geoblock_action` is the authority on that split.
    match geoblock_preflight(vars, urls) {
        GeoblockAction::Proceed => {}
        GeoblockAction::Warn(m) => {
            tracing::warn!(target: "vike_polymarket::adapter", "{m}")
        }
        GeoblockAction::Refuse(m) => {
            tracing::error!(target: "vike_polymarket::adapter", "{m}");
            return Err(None);
        }
    }
    let funder = if loaded.address.is_empty() { signer.clone() } else { loaded.address.clone() };
    let mut l2 = PolymarketConfig { address: signer, ..loaded };
    let now = vike_model::time::clock::now_secs();
    // The EXEC start is the one place that may CREATE the wallet's API key: when the derive says the
    // wallet has none, `POST /auth/api-key` makes the first (`ensure_l2_creating`'s doc has the
    // predicate). The recon-only start below keeps the read-only `ensure_l2`.
    match crate::transport::sign::l1::ensure_l2_creating(&mut l2, clob, now) {
        Ok(origin) => {
            tracing::info!(target: "vike_polymarket::adapter", ?origin, "polymarket: L2 API credentials ready")
        }
        Err(e) => {
            tracing::warn!(target: "vike_polymarket::adapter", error = %e.msg, kind = %e.kind(), "polymarket exec unmounted: could not obtain this wallet's L2 API key (derive, and create when the venue says there is none) — proxy down, geo-blocked, a refused signature or a failed creation?");
            return Err(Some(e.kind()));
        }
    }
    let signature_type = signature_type_for_account(creds);
    let registry = OrderVenueIdTracker::new();
    // The shared dynamic tick-size cache (`crate::instrument::tick_regime`), built here for the same reason the
    // registry is: ONE handle, cloned to the exec thread and returned to the caller, so both read the
    // grid the venue teaches. It starts EMPTY and an unknown token prices verbatim, so starting it
    // changes no price until an off-grid reject has actually resolved that token's tick.
    let tick_regime = TickRegime::new();

    // Best-effort pre-resolution of the engine symbol's signing domain (one round-trip). Inert on
    // failure: the lazy source below retries, and a still-unresolvable token rejects its order
    // rather than guessing (see `crate::neg_risk_lookup`). The start passes whatever symbol the node
    // started, which for this account-wide venue may be a placeholder, so only a token id is read.
    let mut seed = HashMap::new();
    if let Some(token) = seed_token.filter(|s| is_outcome_token(s))
        && let Some(nr) = crate::neg_risk_lookup::clob_neg_risk_fetch(clob.to_string())(token)
    {
        seed.insert(token.to_string(), nr);
    }

    let markets = polymarket_execution_markets(settings);
    // Fee-attribution builderCode, resolved ONCE from the workspace `.env`: `attribution_code_from`
    // validates it against the venue's `AttributionMechanic`, then `decode_builder_bytes32` turns the
    // 0x-hex string into the signed field's raw bytes32. Absent/malformed degrades to `[0u8; 32]`
    // (unattributed) rather than failing the start — this venue has no other place to surface that.
    let builder_code = vike_secrets::credentials::attribution_code_from(vars, "polymarket")
        .and_then(decode_builder_bytes32)
        .unwrap_or([0u8; 32]);
    // The tier is always `live`: this venue has no testnet. The line carries the same `venue`,
    // `account` and `tier` fields as every bridge's live-mount line
    // (`crates/vike-ops/tests/venues/live_mount_line_gate.rs`), and the `target` the line has always had.
    tracing::warn!(
        target: "vike_polymarket::adapter",
        venue = crate::VENUE,
        account = %label,
        tier = AccountTier::Live.as_str(),
        %funder,
        sig_type = signature_type.code(),
        user_markets = markets.len(),
        "⚠ REAL-MONEY: polymarket: flags.poly_exec + creds → LIVE MAINNET exec mount (REAL MONEY — no testnet exists)"
    );
    let client = spawn_live(
        PolymarketLiveConfig {
            creds: l2.clone(),
            maker: funder.clone(),
            signature_type,
            neg_risk: NegRiskSource::lookup(seed, clob.to_string()),
            registry: registry.clone(),
            tracker: None,
            user_channel: Some(UserChannelConfig {
                websocket_url: user_websocket,
                markets,
                history_limit: crate::execution::DEFAULT_HISTORY_LIMIT,
            }),
            builder_code,
            // Ack-race close (`venue.polymarket.presubmit_register`, default OFF): pre-register
            // order client id↔derived-id before each submit so a user-WS fill can't outrun the registry. OFF ⇒
            // byte-identical.
            presubmit_register: presubmit_register_enabled(settings),
            // The local submit gate (`venue.polymarket.rate_gate`, default observe-only), read ONCE
            // here from the venue's settings.
            rate_gate: crate::execution::rest::rate_gate_enforced(settings),
            // The dynamic tick-size regime: the exec thread's clone of the cache above. It prices
            // each submit on this token's known grid and re-fetches that grid on an off-grid reject,
            // so a maker whose tick moved under it is corrected instead of refused forever.
            tick_regime: Some(tick_regime.clone()),
        },
        events.clone(),
        clob,
        relayer,
        gamma,
    )
    // The kill switch: the file `vike-tradehub-core` resolved for the process, handed in — not resolved here.
    .with_halt_path(halt_path);
    // The recon client over the SAME derived creds and the SAME registry the exec thread writes —
    // the whole reason this factory exists rather than two independent ones.
    let recon = if want_reconcile {
        recon_client(l2, funder, signature_type, registry, urls)
    } else {
        None
    };
    Ok(PolymarketLiveRuntime { client: Box::new(client), reconcile: recon, tick_regime })
}

/// **Polymarket's reconcile decision, extracted so it can be tested** — `recon_enabled` (the
/// caller's already-resolved `vike_tradehub::reconcile_config::reconcile_gate` verdict, handed to
/// the start as `AdapterRequest::recon_enabled`) AND the venue's own `flags.poly_reconcile`. It moved
/// here from `vike-tradehub-core` with the arm it served (docs/decisions/0096).
///
/// ⚠ **It reads the master gate, and until 2026-09-06 it did not.** The `("polymarket", _)` arm
/// keyed on `polymarket_reconcile_enabled` ALONE, and this was defended everywhere in the tree as "the
/// venue has its own equivalent inner gate". It was never equivalent: the client this arm builds is
/// only ever USED by the driver both roots start under the master gate, so the effective condition
/// was already `flags.poly_reconcile is on AND recon_enabled` — the arm simply performed the venue's
/// authenticated L1→EOA + `/auth/derive-api-key` round trip first and let the handle be dropped
/// when the gate said no. That is the exact build-then-discard defect
/// [`vike_bridge_core::venue_adapter::reconcile_if_enabled`] exists to have removed for the other five
/// inline venues, wearing a different name.
///
/// ⚠ **And S2 is what made the difference visible, in the direction an operator feels.** The master
/// gate is now ON BY DEFAULT for a start that arms a live venue account, so an account that runs
/// (`flags.poly_exec` on) with `flags.poly_reconcile` on reconciles with nothing else typed —
/// authenticated Polygon-MAINNET reads against a real-money venue with no
/// testnet. That is a real change and it is NAMED rather than smoothed over:
/// `docs/decisions/0043-reconcile-is-on-by-default-under-quarantine.md` carries the verdict and the
/// alternative the owner may prefer, and `docs/ops/reconcile-on-restart.md` tells the operator
/// which single line turns it back off (`flags.poly_reconcile` off, or `flags.reconcile_off`).
/// What has NOT changed is that Polymarket still needs an act nobody else needs: every other venue
/// reconciles on the default alone, this one needs `flags.poly_reconcile` on top of it.
///
/// It was feature-gated with the arm in `vike-tradehub-core`, whose default build compiled no
/// `("polymarket", _)` arm to call it; here it compiles with the rest of `execution_plane`, under this
/// crate's `polymarket` feature.
#[must_use]
fn polymarket_reconcile_wanted(recon_enabled: bool, poly_reconcile: bool) -> bool {
    recon_enabled && poly_reconcile
}

/// The EXEC decision and the real-money factory it guards — the arm's `exec_permitted`
/// conjunction, the one warning it gave, and its call — over the start's own inputs. A function
/// rather than inline in `VenueAdapterPolymarket::start`, with the factory a closure, so the DECISION
/// is testable offline: below `live`, and with `flags.poly_exec` off, the factory — key derivation,
/// the geoblock pre-flight, the L2 round-trip — is never CALLED, which no assertion on the returned
/// `Result` could see. Not permitted is `Err(None)`: refused before any venue call.
fn execution_if_permitted(
    inputs: &AdapterInputs<'_>,
    factory: impl FnOnce() -> Result<PolymarketLiveRuntime, Option<ErrorKind>>,
) -> Result<PolymarketLiveRuntime, Option<ErrorKind>> {
    // ⚠ THE ACCOUNT'S TIER AND `flags.poly_exec` ARE BOTH REQUIRED, and neither replaces the other.
    // An active `live` polymarket row does NOT arm exec — the venue's own double gate stands and
    // `flags.poly_exec` is still mandatory. What the tier adds is the refusal in the other
    // direction, which is the one that was missing: `paper` overrides `flags.poly_exec` outright,
    // and it does so from ABOVE this start (`vike-tradehub-core` returns the paper engine before it asks any
    // bridge), so `polymarket_execution_enabled` — which reads the `flags.poly_exec` the daemon resolved
    // — is never even consulted. A gate that ran after it could be argued with; one that runs
    // before it cannot.
    //
    // `demo` is refused here rather than at that early return, because it is a venue-SPECIFIC fact
    // and this is the venue: Polymarket has NO testnet, so `demo` names a tier that does not exist
    // and the only tier the start could honour is REAL MONEY on Polygon mainnet. Arming it would be
    // a `demo` account trading live, which the account's tier exists to make impossible.
    //
    // Under `demo` and `paper` nothing in this start runs at all: `resolve` answers `LiveOnlyArm`,
    // and `crates/vike-gatewayhub/src/trade/adapters/contract.rs`'s `contract_parts` does not call
    // `start` for an account the probe does not arm.
    let execution_flag = polymarket_execution_enabled(&inputs.flags);
    let live = match inputs.tier {
        AccountTier::Live => true,
        AccountTier::Demo | AccountTier::Paper => false,
    };
    if execution_flag && !live {
        // `paper` never reaches a bridge and `live` permits exec, so the refused tier here is
        // always `demo`. It is recorded through `%` (Display), as the arm recorded its `AccountTier`:
        // a bare `&str` field renders QUOTED on the console layer, so `tier = "demo"` would
        // change the line.
        tracing::warn!(
            venue = crate::VENUE,
            tier = %inputs.tier,
            "flags.poly_exec is on, but this polymarket account's tier is not `live` → exec \
             is NOT RUNNING. Polymarket has no testnet: `demo` names a tier that does not exist \
             here, so the tier refuses rather than arming the only tier there is (real money on \
             Polygon mainnet). Set the account row to `live` to allow it: `vike-cli account \
             ls` for its id, then `vike-cli account set-tier --id <N> --tier live`."
        );
    }
    if execution_flag && live { factory() } else { Err(None) }
}

/// A start's outcome from what the exec factory returned — the arm's `match started`, pure over its
/// inputs so both halves are testable offline. `reconcile_only` is the RECON-ONLY reconcile factory,
/// called only when no exec started and reconcile was asked for.
///
/// ⚠ `PolymarketLiveRuntime::tick_regime` is dropped here exactly as the arm dropped it: the contract has
/// no field for it, nothing outside the exec thread ever consumed it, and the exec thread keeps its
/// own clone of the SAME handle, so the self-heal is unaffected.
fn outcome_of(
    runtime: Result<PolymarketLiveRuntime, Option<ErrorKind>>,
    want_reconcile: bool,
    reconcile_only: impl FnOnce() -> Option<Box<dyn ReconcileClient>>,
) -> AdapterOutcome {
    match runtime {
        Ok(PolymarketLiveRuntime { client, reconcile: recon, tick_regime: _ }) => {
            // LIVE, REAL-MONEY MAINNET exec (the factory already warned, with the funder).
            if want_reconcile && recon.is_none() {
                tracing::warn!(
                    "polymarket: exec mounted but no reconcile client could be built → \
                     reconcile-inert (exec unaffected)"
                );
            }
            AdapterOutcome {
                exec: VenueAdapterExecutionOutcome::Running(VenueAdapterExecutionRunning {
                    client,
                    // The only tier there is. `vike-tradehub-core` keys its identity record on it, and this
                    // venue's reconcile client names no account (`fetch_account_identity` keeps the
                    // trait's `Ok(None)`), so nothing is recorded — as under the arm, which handed
                    // the fold `Demo` here (the CEX-only mainnet conjunct) to the same effect.
                    bound_tier: AccountTier::Live,
                    // No grid of the shape `RiskLimits` reads, so the permissive default stands.
                    grid: None,
                    contract_value: None,
                    // `Cash`, this venue's only mode (`vike_venues::capabilities_for("polymarket")`): a share is
                    // bought outright with the wallet's pUSD and never liquidated. Booked `Cross` (the
                    // `None` the port carried over), the margin-call sweep judged a holding against a
                    // wallet NET of its cost and could sell it at a flat price. The admitting gate stops
                    // charging a held share, which the wallet already paid for.
                    margin_mode: Some(vike_model::MarginMode::Cash),
                    leg_grids: Vec::new(),
                }),
                reconcile: recon,
                identity: None,
            }
        }
        Err(failed) => {
            // Exec off, or it could not be built: absent creds, a bad key, an L2 derivation
            // failure, or a REFUSED region pre-flight (the venue's own geoblock endpoint saying this
            // egress may not place orders — logged at `error!` rather than `warn!`, because that
            // one is a verdict rather than an absence). Each logged its own reason inside the
            // factory. The answer is `VenueAdapterExecutionOutcome::NotRunning`, or `Unreachable`
            // when the L2 handshake failed in a way a later try can pass (`AdapterOutcome::failed`:
            // a network failure, a 5xx, a rate limit; asked again until it starts), which
            // `crates/vike-gatewayhub/src/trade/adapters/contract.rs`'s `contract_parts` turns into
            // NOT RUNNING: no paper client in its place, and the `recon` handed back is dropped.
            let recon = if want_reconcile {
                let recon = reconcile_only();
                if recon.is_some() {
                    tracing::warn!(
                        "polymarket: exec did not start, so this account is NOT RUNNING and the \
                         reconcile client built for it is dropped: flags.poly_reconcile reconciles \
                         only an account whose exec runs. The reason exec did not start is logged \
                         above"
                    );
                } else {
                    tracing::warn!(
                        "polymarket: flags.poly_reconcile is on but no reconcile client could be \
                         built → reconcile-inert"
                    );
                }
                recon
            } else {
                None
            };
            AdapterOutcome {
                reconcile: recon,
                ..failed.map_or_else(AdapterOutcome::paper, AdapterOutcome::failed)
            }
        }
    }
}

/// polymarket's VENUE ADAPTER. `vike_tradehub::registry::REGISTRY` holds `&VenueAdapterPolymarket`
/// under that crate's `polymarket` feature; a build without it registers the venue `FeatureAbsent`
/// and starts it paper.
///
/// ACCOUNT-WIDE, like the recon half: orders, fills, positions and balance all key off the wallet,
/// not the engine symbol, which is used for exactly one thing — a best-effort pre-resolution of
/// that token's NegRisk signing domain, inert on failure ([`live_runtime_for_account`]'s
/// `seed_token`).
pub struct VenueAdapterPolymarket;

impl VenueAdapter for VenueAdapterPolymarket {
    fn venue(&self) -> &'static str {
        crate::VENUE
    }

    fn declaration(&self) -> VenueAdapterDeclaration {
        VenueAdapterDeclaration {
            // `crate::config`'s `load_polymarket_credentials_from` reads this account's OWN fields
            // (`AdapterInputs::credentials`); no path reads the default wallet's key for a labelled
            // account.
            addresses_accounts: true,
            process_exclusive: None,
            // Interval-only reconcile: `vike_tradehub_core::assemble_runtime` threads no reconnect trigger here, and
            // polymarket is not in the node's `reconcile_feed_statuses`, so its health gate reads
            // Healthy.
            // Shares are whole and price is a probability: the venue publishes no instrument grid
            // of the shape `RiskLimits` reads, so the start fetches none.
            grid_source: GridSource::NoGrid,
            // `crate::config`'s `load_polymarket_credentials_from`: the account's `ADDRESS` field (else its
            // `FUNDER`, in the order the loader reads them) is the funder/maker wallet — the thing
            // that HOLDS the outcome tokens — and its `PRIVATE_KEY` is the L1 root that derives the
            // L2 trio.
            book_identity: BookIdentityRule::Named {
                name_fields: &[crate::config::ADDRESS, crate::config::FUNDER],
                evm_key_fields: &[crate::config::PRIVATE_KEY],
            },
            // ⚠ Outcome ④ — the ONE row whose gap is genuinely OURS and genuinely costs orders:
            // `crate::transport::sign::l2`'s `l2_auth_headers` signs `POLY_TIMESTAMP` into every
            // authenticated CLOB request (and `crate::transport::sign::l1`'s `l1_headers` does the same
            // for the key-derivation handshake), so this venue's clock IS on the order path. It is
            // still not read: the CLOB is geo-blocked from the hosts the preflight runs on and is
            // reachable only through the SOCKS egress proxy [`live_runtime_for_account`] builds well
            // AFTER that step. The row can only ever print for a start about to run the LIVE exec
            // client — `vike-tradehub-core`'s clock leg lists a venue only when the arming projection arms
            // it, and [`VenueAdapterPolymarket::resolve`] arms nothing without `flags.poly_exec` and
            // keys — never for a recon-only one. A future lane that moves the proxy earlier should
            // make this `Wired`.
            //
            // ⚠ This row used to read "started recon-only behind a feature here" and render N/A,
            // and it could not print in any configuration where that sentence was true: the old
            // text was false exactly when it appeared, and it dressed the roster's one
            // order-affecting gap as "nothing to see here". The row lived in `vike-tradehub-core`'s clock
            // table until the venue-adapter contract moved it here.
            clock: ClockDeclaration::NotWired {
                reason: "its CLOB is reachable only through the SOCKS egress proxy that the live mount \
                         builds after this step, so there is nothing this preflight can read yet",
                unmeasured_risk: Some(
                    "polymarket signs POLY_TIMESTAMP into every authenticated CLOB request, so a \
                     drifted host clock is on the ORDER path here and this leg does not measure it",
                ),
            },
        }
    }

    /// The arming-probe row as it was, conjunct by conjunct: the ceiling, then `flags.poly_exec`,
    /// then a key. A live session needs BOTH halves the factory itself gates on before any
    /// network: the operator's explicit `flags.poly_exec` AND key material present
    /// ([`live_runtime_for_account`] → the same `load_polymarket_credentials_from` over the account's
    /// fields, `None` on an unset private key). The flag ALONE is not intent — with no key nothing
    /// can ever start live, and the pinned contract is that it does not run and offline. A
    /// PRESENT-but-unusable key
    /// IS intent (the same stance as the hyperliquid row): the factory would decline it into paper,
    /// but a key the operator wrote plus the explicit exec flag must refuse over a missing budget,
    /// not silently trade paper.
    ///
    /// ⚠ …plus the ceiling: this venue has no testnet, so its start refuses anything below `live`
    /// outright, and a `demo`-capped polymarket can only be paper — reporting a session for it
    /// would refuse a start over a budget it cannot need.
    fn resolve(&self, inputs: &AdapterInputs<'_>) -> ArmingResolution {
        if matches!(inputs.tier, AccountTier::Demo | AccountTier::Paper) {
            ArmingResolution::NotRunning(ArmingNotRunningCause::LiveOnlyArm)
        } else if !polymarket_execution_enabled(&inputs.flags) {
            ArmingResolution::NotRunning(ArmingNotRunningCause::ExecFlagUnset)
        } else if crate::config::load_polymarket_credentials_from(&inputs.credentials).is_some() {
            ArmingResolution::Armed { tier: AccountTier::Live, held_below_live: None }
        } else {
            ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials)
        }
    }

    fn start(&self, req: AdapterRequest<'_>) -> AdapterOutcome {
        let vars = req.inputs.secrets;
        let account = req.inputs.account;
        let creds = req.inputs.credentials;
        // BOTH gates, and the master one is not decoration here: it is what stops this start doing
        // the venue's authenticated L2 round trip to build a handle the driver will never be
        // started to use. See [`polymarket_reconcile_wanted`] for the defect that spelling removes and for
        // what S2 changed.
        let want_reconcile = polymarket_reconcile_wanted(
            req.reconcile_enabled,
            polymarket_reconcile_enabled(&req.inputs.flags),
        );
        let runtime = execution_if_permitted(&req.inputs, || {
            // ⚠ Every credential AND the wallet's signature type come from THIS account's own
            // fields. On a venue with no testnet that is the whole safety property — a labelled
            // account may never sign with the default wallet's key.
            live_runtime_for_account(
                &creds,
                vars,
                account,
                Some(req.symbol),
                want_reconcile,
                req.events,
                req.inputs.settings,
                req.inputs.process.halt_path.clone(),
            )
        });
        outcome_of(runtime, want_reconcile, || {
            reconcile_client_for_account(&creds, req.inputs.settings.urls())
        })
    }

    /// The operator's READ of this wallet: the pUSD balance with the allowance it has given each
    /// exchange, and the CLOB's own list of its open orders ([`crate::reconcile::venue_read`]). It needs
    /// neither `flags.poly_reconcile` nor a running pass (a pass folds what it finds; this folds nothing)
    /// and starts no exec client: one derive of the wallet's own L2 key and one signed GET, on the
    /// account's own fields only.
    fn venue_read(
        &self,
        inputs: &AdapterInputs<'_>,
        kind: vike_model::VenueReadKind,
    ) -> Result<vike_model::VenueRead, vike_utility::errors::CarriedRefusal> {
        crate::reconcile::venue_read::venue_read(&inputs.credentials, kind, inputs.settings.urls())
            .map_err(|refusal| refusal.carried())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::refusals::PolymarketStartRefusalId;
    use std::assert_matches;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
    }

    /// The venue's MEASURED refusal body, through a German exit (2026-08-23) — the same fixture
    /// `crate::transport`'s parser tests carry, trimmed to the two fields a refusal must name.
    const BLOCKED_DE: &str = r#"{"blocked":true,"country":"DE","region":"SN"}"#;

    /// The exec gate is OFF by default: it is `flags.poly_exec` as the root resolved it (`AdapterFlags`;
    /// the root's exact-`"1"` read is the gatewayhub's `adapter_flags`), and no other flag arms it.
    #[test]
    fn execution_gate_is_off_by_default() {
        assert!(!polymarket_execution_enabled(&AdapterFlags::default()));
        let on = AdapterFlags { poly_exec: true, ..AdapterFlags::default() };
        assert!(polymarket_execution_enabled(&on));
        let reconcile_only = AdapterFlags { poly_reconcile: true, ..AdapterFlags::default() };
        assert!(!polymarket_execution_enabled(&reconcile_only), "another flag is not this gate");
    }

    /// The venue's settings holding one machine-scoped `venue.polymarket.<field>` row — the shape
    /// the composition root reads out of the settings database (decision 0095).
    fn settings(field: &str, value: &str) -> VenueSettings {
        VenueSettings::from_rows(
            "polymarket",
            &[vike_secrets::VenueSettingRow {
                venue: "polymarket".to_string(),
                tier: None,
                field: vike_venues::venue_fields::VenueFieldName::normalised(field)
                    .store_spelling(),
                value: value.to_string(),
            }],
        )
    }

    /// The pre-submit-registration gate is OFF by default and reads its row typed: the trimmed EXACT
    /// `1` only (`VenueSettings::get_bool` over the field's `ExactOne` grammar), so an unset flag leaves
    /// the exec thread's pre-registration off and behavior byte-identical to before, and an annotated
    /// `1 # …` (which `config set` refuses to write) is no longer on. Read from the
    /// `venue.polymarket.presubmit_register` row (decision 0095, Task 7).
    #[test]
    fn presubmit_register_gate_is_off_by_default_and_exact() {
        let f = "presubmit_register";
        assert!(!presubmit_register_enabled(&VenueSettings::default()));
        assert!(presubmit_register_enabled(&settings(f, "1")));
        assert!(presubmit_register_enabled(&settings(f, " 1 ")));
        for off in ["0", "true", "yes", "on", "", "11", "1x", "1  # derive==orderID live-verified"]
        {
            assert!(!presubmit_register_enabled(&settings(f, off)), "{off}");
        }
        assert!(!presubmit_register_enabled(&settings("rate_gate", "1")), "another field's row");
    }

    /// The escape hatch is OFF by default and accepts the trimmed EXACT `1` only: an annotated
    /// `1 # …` no longer skips the pre-flight (E20, the hole decision 0095's review closed for the exec flag).
    #[test]
    fn geoblock_override_is_off_by_default_and_exact() {
        let k = POLYMARKET_GEOBLOCK_OVERRIDE_ENV;
        assert!(!geoblock_override_enabled(&vars(&[])));
        assert!(geoblock_override_enabled(&vars(&[(k, "1")])));
        assert!(geoblock_override_enabled(&vars(&[(k, " 1 ")])));
        for off in
            ["0", "true", "yes", "on", "", "11", "1x", "1  # DE is close-only, we only flatten"]
        {
            assert!(!geoblock_override_enabled(&vars(&[(k, off)])), "{off}");
        }
    }

    /// A `blocked` verdict REFUSES, and the refusal has to be actionable on its own: the country,
    /// the region, the fact that reads still work, and the way out. An operator reading only this
    /// line must not conclude their credentials or their feeds are broken.
    #[test]
    fn a_blocked_verdict_refuses_and_names_the_country_and_region() {
        let g = crate::transport::parse_geoblock(BLOCKED_DE).unwrap();
        assert_eq!(
            geoblock_action(GeoblockVerdict::Blocked(g.clone())),
            GeoblockAction::Refuse(PolymarketStartRefusal::GeoblockBlocked { geoblock: g })
        );
        // the refusal's one literal spells the override switch by the constant's own name
        let way_out = format!("{POLYMARKET_GEOBLOCK_OVERRIDE_ENV}=1");
        assert!(PolymarketStartRefusalId::GeoblockBlocked.template().contains(&way_out));
    }

    /// THE OTHER HALF OF THE CONTRACT: a probe that could not be reached or could not be read is a
    /// WARNING, never a refusal. A venue outage, a dead tunnel or an HTML error page must not be
    /// able to fail a start — that would be a worse defect than the silent 403 this catches.
    #[test]
    fn an_unreachable_or_unreadable_probe_warns_instead_of_refusing() {
        use crate::transport::{geoblock_verdict, parse_geoblock};
        // a network failure, in the shape `observe_geoblock` reports one
        let net = geoblock_verdict(Err("geoblock probe: dial tcp: timed out".to_string()));
        assert_eq!(
            geoblock_action(net),
            GeoblockAction::Warn(PolymarketStartRefusal::GeoblockUnread {
                why: "geoblock probe: dial tcp: timed out".to_string()
            })
        );
        // an unparseable body (a proxy's error page, say) takes the same road
        let junk = geoblock_verdict(parse_geoblock("<html>502 Bad Gateway</html>"));
        assert_matches!(geoblock_action(junk), GeoblockAction::Warn(_));
        // ...while an explicit all-clear is silent
        let ok = geoblock_verdict(parse_geoblock(r#"{"blocked":false,"country":"IE"}"#));
        assert_eq!(geoblock_action(ok), GeoblockAction::Proceed);
    }

    /// The override short-circuits the probe — no network — and is still LOUD: a skipped
    /// pre-flight and a passed one must never read the same in a log.
    #[test]
    fn the_override_skips_the_preflight_offline_and_says_so() {
        let overridden = vars(&[(POLYMARKET_GEOBLOCK_OVERRIDE_ENV, "1")]);
        // No address rows at all: the override needs none.
        assert_eq!(
            geoblock_preflight(&overridden, &VenueUrls::default()),
            GeoblockAction::Warn(PolymarketStartRefusal::GeoblockSkipped),
            "the override must proceed with a warning, not silently"
        );
        let switch = format!("{POLYMARKET_GEOBLOCK_OVERRIDE_ENV}=1");
        assert!(PolymarketStartRefusalId::GeoblockSkipped.template().contains(&switch));
    }

    /// The `venue.polymarket.exec_markets` row (decision 0095, Task 7): empty is account-wide, and
    /// both separators parse.
    #[test]
    fn markets_default_to_account_wide_and_parse_both_separators() {
        let f = "exec_markets";
        assert!(polymarket_execution_markets(&VenueSettings::default()).is_empty());
        assert_eq!(polymarket_execution_markets(&settings(f, "0xaa,0xbb")), vec!["0xaa", "0xbb"]);
        assert_eq!(
            polymarket_execution_markets(&settings(f, "0xaa 0xbb,  0xcc")),
            vec!["0xaa", "0xbb", "0xcc"]
        );
        // a trailing `.env` comment is annotation, not a market id
        assert_eq!(
            polymarket_execution_markets(&settings(f, "0xaa  # the 5m BTC updown")),
            vec!["0xaa"]
        );
        assert!(polymarket_execution_markets(&settings(f, "   ")).is_empty());
    }

    /// Absent / unusable creds ⇒ `Err(None)` (refused before any venue call) and **no network call** (the
    /// CI-safe half: this test must return before the `ensure_l2` round-trip, exactly like
    /// `reconcile_client_for_account`'s twin).
    #[test]
    fn without_a_usable_key_the_start_is_none_and_offline() {
        use vike_secrets::account_credentials::CredentialFields;
        let (tx, _rx) = vike_execution::event_channel(8);
        let none = VenueSettings::default();
        let deployment = vars(&[]);
        // No client is ever built on these paths, so the sentinel is never looked at.
        let no_halt = std::path::PathBuf::new;
        let runtime = |fields: &[(&str, &str)]| {
            let fields = CredentialFields::from_pairs(fields.iter().copied());
            let creds = AccountCredentials::new(Some(1), Some(&fields), None);
            let label = AccountLabel::Default;
            live_runtime_for_account(&creds, &deployment, &label, None, true, &tx, &none, no_halt())
        };
        assert!(matches!(runtime(&[]), Err(None)));
        assert!(matches!(runtime(&[("FUNDER", "0xabc")]), Err(None)));
        assert!(matches!(runtime(&[("PRIVATE_KEY", "not-a-key")]), Err(None)));
    }

    /// **A usable key with no address rows does NOT RUN, and dials nothing**: the lookups come
    /// before the geoblock probe and the L2 handshake, so a missing row is refused offline (with
    /// the rows a start dials, the same key would reach the network).
    #[test]
    fn a_usable_key_without_address_rows_is_not_running_and_offline() {
        use vike_secrets::account_credentials::CredentialFields;
        let (tx, _rx) = vike_execution::event_channel(8);
        let key = "0xc85ef7d79691fe79573b1a7064c19c1a9819ebdbd1faaab1a8ec92344438aaf4";
        let fields = CredentialFields::from_pairs([("PRIVATE_KEY", key)]);
        let creds = AccountCredentials::new(Some(1), Some(&fields), None);
        let no_rows = VenueSettings::default();
        let runtime = live_runtime_for_account(
            &creds,
            &vars(&[]),
            &AccountLabel::Default,
            None,
            true,
            &tx,
            &no_rows,
            std::path::PathBuf::new(),
        );
        assert!(matches!(runtime, Err(None)));
    }

    /// The release's polymarket rows with the CLOB's (`live trade rest`) pointed at `clob`.
    fn rows_with_clob(clob: &str) -> VenueSettings {
        use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlType, VENUE_URL_SEED};
        let rows = VENUE_URL_SEED.iter().filter(|s| s.venue == crate::VENUE).map(|s| {
            let is_clob = (s.ty, s.channel, s.protocol, s.product)
                == (UrlType::Live, UrlChannel::Trade, UrlProtocol::Rest, "");
            (s.venue, s.ty, s.channel, s.protocol, s.product, if is_clob { clob } else { s.url })
        });
        VenueSettings::default().with_urls(VenueUrls::from_rows(crate::VENUE, rows))
    }

    /// **A start whose L2 handshake fails is told apart by the handshake's own kind** (owner, 2026-11-16):
    /// an unreachable CLOB is `Unreachable`, asked again until it starts, and a CLOB that declines the
    /// ClobAuth signature (`401`) is `NotRunning`, which waits for the operator. The geoblock pre-flight is
    /// skipped by its override and the CLOB row is a loopback address, so nothing leaves this host, and
    /// neither start gets past the handshake (nothing is spawned).
    #[test]
    fn a_start_whose_handshake_fails_on_the_network_is_asked_again_and_a_declined_one_parks() {
        use crate::execution::test_support::{FakeServer, Reply, declare_direct_egress};
        use vike_secrets::account_credentials::CredentialFields;
        declare_direct_egress();
        let declined =
            FakeServer::start(|_| Reply::text(401, r#"{"error":"Invalid L1 Request headers"}"#));
        let (tx, _rx) = vike_execution::event_channel(8);
        let key = "0xc85ef7d79691fe79573b1a7064c19c1a9819ebdbd1faaab1a8ec92344438aaf4";
        let fields = CredentialFields::from_pairs([("PRIVATE_KEY", key)]);
        let creds = AccountCredentials::new(Some(1), Some(&fields), None);
        let deployment = vars(&[(POLYMARKET_GEOBLOCK_OVERRIDE_ENV, "1")]);
        for (clob, asked_again) in [("http://127.0.0.1:1", true), (declined.base.as_str(), false)] {
            let runtime = live_runtime_for_account(
                &creds,
                &deployment,
                &AccountLabel::Default,
                None,
                false,
                &tx,
                &rows_with_clob(clob),
                std::path::PathBuf::new(),
            );
            let out = outcome_of(runtime, false, || -> Option<Box<dyn ReconcileClient>> {
                panic!("reconcile was not asked for")
            });
            let unreachable = matches!(out.exec, VenueAdapterExecutionOutcome::Unreachable);
            let parked = matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning);
            assert_eq!((unreachable, parked), (asked_again, !asked_again), "{clob}");
            assert!(out.reconcile.is_none() && out.identity.is_none(), "{clob}");
        }
    }

    #[test]
    fn token_id_shape_gate() {
        assert!(is_outcome_token(&"7".repeat(70)));
        assert!(!is_outcome_token("BTCUSDT"));
        assert!(!is_outcome_token("123")); // too short to be a uint256 token id
        assert!(!is_outcome_token(&"7".repeat(79))); // more digits than a uint256 holds
        assert!(!is_outcome_token(&format!("0x{}", "a".repeat(64)))); // a condition id, not a token id
    }
}

#[path = "adapter_contract_tests.rs"]
#[cfg(test)]
mod adapter_contract_tests;
