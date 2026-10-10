//! The Polymarket VENUE MOUNT — [`PolymarketVenueMount`], this venue's implementation of the
//! `vike_bridge_core::venue_mount::VenueMount` contract
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md), and the live-mount
//! factory it calls, so no signer / derivation / address / websocket detail lives in the
//! composition root (the same discipline
//! [`crate::exec_plane::recon_client::recon_client_from_vars`] follows for the recon half).
//! `vike-mount`'s `("polymarket", _)` arm, its arming-probe row and its clock, book-identity and
//! grid-source rows moved here, behaviour unchanged. The egress the factory dials through is the
//! process-wide `declare_egress` cell the composition root fills from the settings database —
//! the one named exception to "a bridge reads no process global"; the mount reads no environment
//! and no store.
//!
//! [`live_mount_from_vars`] returns the exec client AND the recon client **built over ONE shared
//! [`PolymarketRegistry`] and ONE L2 handshake**. That sharing is the point: the CLOB assigns order
//! ids and never echoes a client id, so a reconcile report can only name a local coid if it reads
//! the very map the exec thread writes. Before this existed, `make_engine` built the recon client
//! with a FRESH empty registry and every order report came back `client_order_id: None`.
//!
//! ## What is gated, and why there are two gates
//! - `flags.poly_exec` ([`poly_exec_enabled`]) — the exec gate. Polymarket has **no testnet**: every
//!   order this mounts is real money on Polygon mainnet, so the venue is opt-in twice over (creds
//!   AND this flag) rather than the usual once. Off ⇒ [`PolymarketVenueMount`] builds nothing here,
//!   makes no network call, and the venue stays PAPER — byte-identical to before this existed.
//! - `flags.poly_reconcile` ([`crate::exec_plane::recon_client::poly_reconcile_enabled`]) — unchanged, still gates
//!   the reconcile half independently. The two compose: exec-only, recon-only, both, or neither.
//!
//! A THIRD flag, `venue.polymarket.presubmit_register` ([`presubmit_register_enabled`]), is NOT a
//! mount gate but a within-exec behavior toggle threaded onto
//! [`PolymarketLiveConfig::presubmit_register`]: ON,
//! the exec thread pre-registers `coid`↔`derive_order_id(&order)` just before each submit to close
//! the ack-race (a user-WS fill can beat the HTTP ack). Default OFF ⇒ byte-identical to today.
//!
//! None of them is read from the environment (decision 0095). The two flags are read from the
//! caller-supplied map, which the daemon folds them into; `presubmit_register`, `exec_markets` and
//! the rate gate are `venue.polymarket.*` rows read from the venue's settings
//! (`MountInputs::settings`) — the store folded them into the credential map under legacy names
//! until decision 0095's Task 7 retired that fold.
//!
//! ## The region pre-flight (why an exec mount can now REFUSE)
//!
//! Polymarket restricts ORDER PLACEMENT by region and nothing else: from a restricted egress the
//! market feeds stream, `/auth/derive-api-key` succeeds and every authenticated READ — balance,
//! positions, orders, fills — comes back green, while a submit returns a 403 saying trading is
//! restricted in your region. A wrong-region session therefore looked perfectly healthy right up
//! to the moment it tried to trade, which on an exec path is the worst possible time to find out.
//!
//! [`live_mount_from_vars`] now asks the venue first, through [`crate::egress::GEOBLOCK_URL`] and
//! the SAME resolved proxy the orders will use, and REFUSES the live exec mount when the answer is
//! `blocked` — naming the country and region, and saying that reads are unaffected. It refuses on
//! a POSITIVE answer only: an unreachable probe or an unreadable body warns and proceeds
//! ([`geoblock_action`]). The check is EXEC-ONLY on purpose — reads are legal from a restricted
//! region, so gating the feeds on it would break the one setup that is legitimately region-blind.
//!
//! A FOURTH flag, `POLY_GEOBLOCK_OVERRIDE` = `1` ([`geoblock_override_enabled`]), skips it — a
//! credential-store row, like the three above it, and its doc carries the argument.
//!
//! The **dynamic tick-size regime** ([`crate::tick_regime`]) is deliberately NOT a flag at all: this
//! factory always builds one and threads it into the exec thread, because it exists to repair a live
//! failure the venue reports only as an endless stream of rejects, and an operator cannot know to
//! turn it on before hitting that. Mounting it is safe to do unconditionally because it starts EMPTY
//! and an unresolved token prices verbatim, so no price moves until an off-grid reject has actually
//! taught it that token's grid. It rides `flags.poly_exec` — an exec-less mount builds nothing here.

use std::collections::HashMap;

use vike_bridge_core::credentials::Environment;
use vike_bridge_core::venue_mount::{
    BookIdentity, ClockDecl, DeclaredGridSource, ExecOutcome, LiveExec, MountInputs, MountOutcome,
    MountRequest, PaperCause, Resolution, Tier, VenueDeclaration, VenueMount,
};
use vike_exec::recon::ReconClient;
use vike_exec::{EventSender, ExecutionClient};

use crate::config::{CLOB_BASE, PolymarketCreds, WS_USER, first_token};
use crate::egress::{GeoblockVerdict, check_order_placement_geo};
use crate::exec_plane::client::{
    PolymarketExecutionClient, PolymarketLiveConfig, UserChannelConfig, decode_builder_bytes32,
};
use crate::exec_plane::recon_client::{
    poly_reconcile_enabled, recon_client, recon_client_for_account, signature_type_for_account,
};
use crate::exec_plane::registry::PolymarketRegistry;
use crate::neg_risk_lookup::NegRiskSource;
use crate::tick_regime::TickRegime;
use vike_secrets::venue_setting::{SettingTier, VenueSettings};

/// The name the daemon folds `flags.poly_exec` into the credential map under.
pub const POLY_EXEC_ENV: &str = "POLY_EXEC";
/// The credential-store name of the geoblock pre-flight's escape hatch (see
/// [`geoblock_override_enabled`]).
pub const POLY_GEOBLOCK_OVERRIDE_ENV: &str = "POLY_GEOBLOCK_OVERRIDE";

/// The exec opt-in: the EXACT string `"1"` (the `VIKE_RECONCILE` idiom — not a fuzzy truthy parse),
/// in the caller's map. Default OFF.
///
/// This is a SECOND gate on top of absent-credentials-is-the-live-gate, and it exists because
/// Polymarket is the one mounted venue with no demo tier at all: on every other venue a mis-set
/// credential mounts a *demo* client, here it mounts a real one. `POLY_EXEC` unset means an operator
/// who merely has keys in their `.env` — which the reconcile half, the data feeds and the redeem
/// poller all legitimately want — cannot accidentally arm order placement.
pub fn poly_exec_enabled(vars: &HashMap<String, String>) -> bool {
    vars.get(POLY_EXEC_ENV).map(|v| first_token(v)) == Some("1")
}

/// The pre-submit-registration opt-in: the EXACT string `"1"` (the `POLY_EXEC` / `VIKE_RECONCILE`
/// idiom — not a fuzzy truthy parse) of `venue.polymarket.presubmit_register`, read from the
/// venue's settings (decision 0095). Default OFF.
///
/// ON ⇒ the exec thread pre-registers `coid`↔`crate::exec_plane::order::derive_order_id(&order)` in the shared
/// registry just BEFORE each network submit, closing the ack-race in which a user-WS fill/cancel
/// beats the HTTP acceptance and lands keyed by an order id the registry does not yet hold (the
/// `client_order_id: None` path). Safe because that derived id is the CLOB's own `orderID` — the
/// venue keys orders by the EIP-712 order hash — and the later real `OrderAccepted` re-registers the
/// SAME `(coid, id, side)` as an idempotent no-op (see [`PolymarketRegistry::on_accept`]).
///
/// Kept a SEPARATE gate from `POLY_EXEC` (rather than always-on with exec) as verify-before-enable
/// discipline: the `derive_order_id == orderID` premise is LIVE-VERIFIED on a real demo order
/// (`crate::exec_plane::client::tests::live_place_and_cancel`), but booking a pre-registered id into the money
/// path is opt-in until that smoke is re-run against the deployment. Unset (the default) ⇒ nothing
/// is registered pre-submit — byte-identical to before this existed.
///
/// ⚠ **This flag is no longer the only thing standing between a raced fill and a silent loss, and
/// must not be read as such.** It ships OFF, and while it was the sole mitigation an executed fill
/// that beat the ack was simply discarded at `crate::exec_plane::user_ws`'s re-key sites — position and PnL
/// silently wrong. The unconditional staging in [`crate::exec_plane::pending_events`] now covers that race in
/// every build, flag or no flag. What the flag still buys is AVOIDING the stage entirely (the id is
/// already there when the frame lands), which is cheaper and needs no TTL to be right — but it is an
/// optimisation over the default now, not the safety net.
pub fn presubmit_register_enabled(settings: &VenueSettings) -> bool {
    settings.get(SettingTier::Any, "presubmit_register").map(first_token) == Some("1")
}

/// The CLOB **condition ids** the user-channel subscribe frame names, from the
/// `venue.polymarket.exec_markets` row in the venue's settings (comma- and/or
/// whitespace-separated).
///
/// ⚠ These are market `condition_id`s, NOT outcome `token_id`s — the two look alike (both long hex/
/// decimal strings) and the user channel silently streams nothing if you hand it token ids. The
/// `markets` field of the subscribe frame is the same one
/// [`user_subscribe_message`](crate::exec_plane::user_ws::user_subscribe_message) takes and the
/// `polymarket_user_smoke` passes a `condition_id` to.
///
/// An EMPTY list (the default) subscribes account-wide — the CLOB user channel treats an empty
/// `markets` array as "every market this key trades", which is what a mount wants when the strategy
/// picks its markets at runtime. That behavior is asserted live by
/// `poly_exec_mount_smoke::empty_markets_is_account_wide`, because it is a server-side convention
/// with no contractual documentation; if a future deployment changes it, set
/// `venue.polymarket.exec_markets` explicitly and the pump narrows to exactly those markets.
/// **Either way the A3 resync is the backstop**: it replays `/data/trades` + `/data/orders`, which
/// are account-wide regardless of the subscribe list, after every reconnect.
pub fn poly_exec_markets(settings: &VenueSettings) -> Vec<String> {
    let raw = settings.get(SettingTier::Any, "exec_markets").unwrap_or_default().to_string();
    raw.split(|c: char| c == ',' || c.is_whitespace())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        // `#` starts a trailing `.env` comment; everything from it on is annotation, not a market.
        .take_while(|s| !s.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// The geoblock pre-flight's escape hatch: the EXACT string `"1"` (the [`poly_exec_enabled`]
/// idiom, trailing-`.env`-comment tolerant), read from the caller-supplied map. Default OFF ⇒ the
/// pre-flight runs.
///
/// Map-only, like every gate in this module since decision 0095. It ARMS nothing on its own:
/// `flags.poly_exec` is still required, and a `POLY_EXEC` credential row or variable refuses
/// startup — all this flag can do is decline a pre-flight, and a genuinely blocked region still
/// refuses each order at submit.
pub fn geoblock_override_enabled(vars: &HashMap<String, String>) -> bool {
    vars.get(POLY_GEOBLOCK_OVERRIDE_ENV).map(|v| first_token(v)) == Some("1")
}

/// What a live exec mount does about the venue's geoblock verdict — the three outcomes, kept as
/// data so [`geoblock_action`] can be decided (and tested) without a logger or a network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeoblockAction {
    /// The venue confirmed this egress may place orders. Mount, say nothing.
    Proceed,
    /// Mount anyway, but say why the pre-flight proved nothing.
    Warn(String),
    /// REFUSE the live exec mount, naming where the venue is refusing from.
    Refuse(String),
}

/// The PURE decision behind the pre-flight — no network, no environment, so BOTH halves of the
/// contract are unit-testable rather than asserted in prose:
///
/// - a `Blocked` verdict **REFUSES** the live exec mount, naming the country and the region and
///   saying explicitly that reads are unaffected (they are: the same egress that gets a 403 at
///   submit fetches balance, positions, orders and fills perfectly well);
/// - an `Unknown` verdict — probe unreachable, body unreadable, venue having a bad day — only
///   **WARNS**. An unreachable pre-flight is evidence of nothing, and a venue outage that could
///   fail a mount would be a worse defect than the silent 403 this exists to catch.
pub fn geoblock_action(verdict: GeoblockVerdict) -> GeoblockAction {
    match verdict {
        GeoblockVerdict::Allowed(_) => GeoblockAction::Proceed,
        GeoblockVerdict::Blocked(g) => GeoblockAction::Refuse(format!(
            "polymarket exec REFUSED: the venue's own geoblock endpoint says ORDER PLACEMENT is \
             blocked from this egress — {g}. Market data and AUTHENTICATED READS (balance, \
             positions, orders, fills) are NOT affected; only order submission is, and it would \
             have failed one order at a time with a 403 on the exec path. Egress from a permitted \
             region (`venue.polymarket.socks_proxy`, or `proxy_host` / `proxy_port`), or set \
             {POLY_GEOBLOCK_OVERRIDE_ENV}=1 in the credential store to mount anyway."
        )),
        GeoblockVerdict::Unknown(why) => GeoblockAction::Warn(format!(
            "polymarket: the geoblock pre-flight could not be reached or read ({why}) — mounting \
             ANYWAY. An unreachable probe is not evidence of a block, so this is deliberately not \
             a refusal; if this region is in fact restricted, the first order will 403 instead."
        )),
    }
}

/// The pre-flight as the mount runs it: the operator override FIRST, then the venue's own answer.
///
/// The override short-circuits the probe rather than overruling its result, the same "inert unless
/// configured" discipline [`crate::egress::check_expected_egress`] follows: an operator who has
/// already answered this question should not pay a blocking round-trip on the mount path for an
/// answer that cannot change anything. It stays LOUD either way — skipping the check is itself a
/// warning, because a skipped pre-flight and a passed one must never read the same in a log.
fn geoblock_preflight(vars: &HashMap<String, String>) -> GeoblockAction {
    if geoblock_override_enabled(vars) {
        return GeoblockAction::Warn(format!(
            "polymarket: {POLY_GEOBLOCK_OVERRIDE_ENV}=1 — the venue geoblock pre-flight is \
             SKIPPED, so this mount is NOT checked against the venue's region policy. A \
             restricted region refuses order placement per order, with a 403 at submit."
        ));
    }
    geoblock_action(check_order_placement_geo())
}

/// A live Polymarket mount: the exec client (with its user-WS return lane already running inside
/// its own exec thread) plus the recon client that shares its registry.
pub struct PolymarketMount {
    /// The client [`PolymarketVenueMount`] hands `vike-mount` as `LiveExec::client`.
    pub client: Box<dyn ExecutionClient + Send>,
    /// `Some` when the caller asked for reconcile AND the L2 trio derived — the SAME creds and the
    /// SAME registry the exec client uses.
    pub recon: Option<Box<dyn ReconClient>>,
    /// The dynamic tick-size cache the exec thread prices orders on ([`crate::tick_regime`]) — the
    /// caller's clone of the SAME handle, so a quoting path can round its own quotes on whatever
    /// grid the venue has taught the exec thread (`Clone` shares one state). Consuming it is
    /// optional; the self-heal works whether or not anyone holds this end.
    pub tick_regime: TickRegime,
}

/// Build the live Polymarket mount straight from the workspace `.env` map.
///
/// Returns `None` — leaving [`PolymarketVenueMount`] on its paper outcome — for every one of:
/// absent `POLY_PRIVATE_KEY` (absent-credentials-is-the-live-gate), an unusable key, a REFUSED
/// region pre-flight (the venue itself says this egress may not place orders — [`geoblock_action`],
/// logged at `error!` rather than `warn!` because it is a verdict rather than an absence), or a
/// failed L2 derivation (tunnel down / geo-blocked / rejected ClobAuth signature). None of those is
/// a mount failure or a panic; each logs its own reason. The gates themselves (`POLY_EXEC` /
/// `POLY_RECONCILE`) are the CALLER's to check — `PolymarketVenueMount::mount` checks them so that
/// an unset flag makes no network call at all.
///
/// What it resolves that a raw `PolymarketExecutionClient::spawn_live` takes as given — the same
/// three things `recon_client_from_vars` documents, because they are the same handshake:
/// 1. **The L1 signer address.** `load_polymarket_creds_from` fills `address` from
///    `POLY_ADDRESS`/`POLY_FUNDER`, which on a deposit-wallet (`POLY_1271`) account is the FUNDER,
///    not the key's EOA. ClobAuth must be signed by, and name, the EOA — so it is re-derived here.
/// 2. **The L2 trio**, via ONE blocking, proxy-routed `/auth/derive-api-key` round-trip at mount,
///    shared by the exec client, its user-WS subscribe frame, and the recon client.
/// 3. **The funder/maker**, which stays `POLY_FUNDER` (the deposit wallet the orders are made for),
///    falling back to the EOA for a bare-key account.
///
/// `seed_token` is an optional outcome `token_id` (the mount's `MountRequest::symbol`) whose
/// NegRisk flag is pre-resolved here so the FIRST order pays no lookup latency; a failure to resolve
/// it is inert — the exec thread's lazy [`NegRiskSource::lookup`] retries per order.
///
/// `settings` is the venue's `venue_setting` rows (`MountInputs::settings`) — the rate gate, the
/// user-channel markets and the pre-submit registration are read from them (decision 0095).
///
/// `halt_path` is the operator HALT sentinel the exec client watches
/// (`MountInputs::process.halt_path`): the factory is handed it, as it is handed everything else
/// the process decided, and resolves nothing itself (decision 0099). A caller with no sentinel — a
/// smoke — passes a path it owns and never creates.
pub fn live_mount_from_vars(
    vars: &HashMap<String, String>,
    seed_token: Option<&str>,
    want_recon: bool,
    events: &EventSender,
    settings: &VenueSettings,
    halt_path: std::path::PathBuf,
) -> Option<PolymarketMount> {
    live_mount_for_account(
        vars,
        &vike_model::accounts::account_keys::AccountLabel::Default,
        seed_token,
        want_recon,
        events,
        settings,
        halt_path,
    )
}

/// [`live_mount_from_vars`] for ONE NAMED ACCOUNT — everything it documents, with every credential
/// and the signature type read through THIS account's key names
/// (`POLY_{TIER}_{SUFFIX}__{LABEL}`, `POLY_SIGNATURE_TYPE__{LABEL}`).
///
/// ⚠ **[`AccountLabel::Default`](vike_model::accounts::account_keys::AccountLabel::Default) is byte-identically
/// [`live_mount_from_vars`]**, reached through it — the grammar returns every key name unchanged for
/// that account.
///
/// ⚠ **No path here reads the default account's key on behalf of a labelled one.** That is what the
/// refusal in `vike_mount::arm_addresses_accounts` used to stand in for: on a REAL-MONEY-only venue,
/// a second engine built on the first wallet's key is two engines trading one account.
///
/// ⚠ What stays DEPLOYMENT-wide, deliberately: `flags.poly_exec` (the caller's to check),
/// `venue.polymarket.exec_markets`, `venue.polymarket.presubmit_register`,
/// `venue.polymarket.rate_gate`, the geoblock pre-flight and the builder attribution code. None of
/// them names a wallet — they describe this process's egress and behaviour, so a second account
/// inherits them by being on the same box. The three `venue.polymarket.*` rows arrive as
/// `settings`, the venue's machine-scoped settings.
pub fn live_mount_for_account(
    vars: &HashMap<String, String>,
    label: &vike_model::accounts::account_keys::AccountLabel,
    seed_token: Option<&str>,
    want_recon: bool,
    events: &EventSender,
    settings: &VenueSettings,
    halt_path: std::path::PathBuf,
) -> Option<PolymarketMount> {
    // Polymarket is Polygon-MAINNET-only — there is no testnet — so the live tier is the only tier.
    let loaded = crate::config::load_polymarket_creds_for_account(Environment::Live, label, vars)?;
    let signer = match vike_bridge_core::eip712::eth_address_from_private_key(&loaded.private_key) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(target: "vike_polymarket::exec_plane::mount", error = %e, "polymarket exec unmounted: POLY_PRIVATE_KEY is not a usable key");
            return None;
        }
    };
    // The venue's OWN region pre-flight, BEFORE the L2 handshake and before a single order is
    // signed — and deliberately AFTER the credential/key gates above, so an unconfigured box still
    // makes no network call at all. Polymarket refuses ORDER PLACEMENT by region while leaving
    // market data and authenticated READS working, so without this a wrong-region session looks
    // completely healthy (it mounts, streams, reconciles positions, shows balances) and learns the
    // truth only as a per-order 403 on the exec path. A block REFUSES here; an unreachable or
    // unreadable probe only warns — `geoblock_action` is the authority on that split.
    match geoblock_preflight(vars) {
        GeoblockAction::Proceed => {}
        GeoblockAction::Warn(m) => {
            tracing::warn!(target: "vike_polymarket::exec_plane::mount", "{m}")
        }
        GeoblockAction::Refuse(m) => {
            tracing::error!(target: "vike_polymarket::exec_plane::mount", "{m}");
            return None;
        }
    }
    let funder = if loaded.address.is_empty() { signer.clone() } else { loaded.address.clone() };
    let mut creds = PolymarketCreds { address: signer, ..loaded };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    if let Err(e) = crate::exec_plane::l1::ensure_l2(&mut creds, CLOB_BASE, now) {
        tracing::warn!(target: "vike_polymarket::exec_plane::mount", error = %e, "polymarket exec unmounted: L2 derivation failed (proxy down / geo-blocked?)");
        return None;
    }
    let signature_type = signature_type_for_account(vars, label);
    let registry = PolymarketRegistry::new();
    // The shared dynamic tick-size cache (`crate::tick_regime`), built here for the same reason the
    // registry is: ONE handle, cloned to the exec thread and returned to the caller, so both read the
    // grid the venue teaches. It starts EMPTY and an unknown token prices verbatim, so mounting it
    // changes no price until an off-grid reject has actually resolved that token's tick.
    let tick_regime = TickRegime::new();

    // Best-effort pre-resolution of the mounted symbol's signing domain (one round-trip). Inert on
    // failure: the lazy source below retries, and a still-unresolvable token rejects its order
    // rather than guessing (see `crate::neg_risk_lookup`).
    let mut seed = HashMap::new();
    if let Some(tok) = seed_token.filter(|s| is_token_id(s))
        && let Some(nr) = crate::neg_risk_lookup::clob_neg_risk_fetch()(tok)
    {
        seed.insert(tok.to_string(), nr);
    }

    let markets = poly_exec_markets(settings);
    // Fee-attribution builderCode, resolved ONCE from the workspace `.env`: `attribution_code_from`
    // validates it against the venue's `AttributionMechanic`, then `decode_builder_bytes32` turns the
    // 0x-hex string into the signed field's raw bytes32. Absent/malformed degrades to `[0u8; 32]`
    // (unattributed) rather than failing the mount — this venue has no other place to surface that.
    let builder_code = vike_bridge_core::credentials::attribution_code_from(vars, "polymarket")
        .and_then(decode_builder_bytes32)
        .unwrap_or([0u8; 32]);
    // The tier is always `live`: this venue has no testnet. The line carries the same `venue`,
    // `account` and `tier` fields as every bridge's live-mount line
    // (`crates/vike-ops/tests/venues/live_mount_line_gate.rs`), and the `target` the line has always had.
    tracing::warn!(
        target: "vike_polymarket::exec_plane::mount",
        venue = crate::VENUE,
        account = %label,
        tier = Tier::Live.as_str(),
        %funder,
        sig_type = signature_type.code(),
        user_markets = markets.len(),
        "⚠ REAL-MONEY: polymarket: flags.poly_exec + creds → LIVE MAINNET exec mount (REAL MONEY — no testnet exists)"
    );
    let client = PolymarketExecutionClient::spawn_live(
        PolymarketLiveConfig {
            creds: creds.clone(),
            maker: funder.clone(),
            signature_type,
            neg_risk: NegRiskSource::lookup(seed),
            registry: registry.clone(),
            tracker: None,
            user_channel: Some(UserChannelConfig {
                ws_url: WS_USER.to_string(),
                markets,
                history_limit: crate::exec_plane::client::DEFAULT_HISTORY_LIMIT,
            }),
            builder_code,
            // Ack-race close (`venue.polymarket.presubmit_register`, default OFF): pre-register
            // coid↔derived-id before each submit so a user-WS fill can't outrun the registry. OFF ⇒
            // byte-identical.
            presubmit_register: presubmit_register_enabled(settings),
            // The local submit gate (`venue.polymarket.rate_gate`, default observe-only), read ONCE
            // here from the venue's settings.
            rate_gate: crate::exec_plane::exec::rate_gate_enforced(settings),
            // The dynamic tick-size regime: the exec thread's clone of the cache above. It prices
            // each submit on this token's known grid and re-fetches that grid on an off-grid reject,
            // so a maker whose tick moved under it is corrected instead of refused forever.
            tick_regime: Some(tick_regime.clone()),
        },
        events.clone(),
    )
    // The kill switch: the file `vike-mount` resolved for the process, handed in — not resolved here.
    .with_halt_path(halt_path);
    // The recon client over the SAME derived creds and the SAME registry the exec thread writes —
    // the whole reason this factory exists rather than two independent ones.
    let recon =
        if want_recon { recon_client(creds, funder, signature_type, registry) } else { None };
    Some(PolymarketMount { client: Box::new(client), recon, tick_regime })
}

/// **Polymarket's reconcile decision, extracted so it can be tested** — `recon_enabled` (the
/// caller's already-resolved `vike_tradehub::reconcile_config::reconcile_gate` verdict, handed to
/// the mount as `MountRequest::recon_enabled`) AND the venue's own `flags.poly_reconcile`. It moved
/// here from `vike-mount` with the arm it served (docs/decisions/0096).
///
/// ⚠ **It reads the master gate, and until 2026-09-06 it did not.** The `("polymarket", _)` arm
/// keyed on `poly_reconcile_enabled` ALONE, and this was defended everywhere in the tree as "the
/// venue has its own equivalent inner gate". It was never equivalent: the client this arm builds is
/// only ever USED by the driver both roots mount under the master gate, so the effective condition
/// was already `flags.poly_reconcile is on AND recon_enabled` — the arm simply performed the venue's
/// authenticated L1→EOA + `/auth/derive-api-key` round trip first and let the handle be dropped
/// when the gate said no. That is the exact build-then-discard defect
/// [`vike_bridge_core::venue_mount::recon_if_enabled`] exists to have removed for the other five
/// inline venues, wearing a different name.
///
/// ⚠ **And S2 is what made the difference visible, in the direction an operator feels.** The master
/// gate is now ON BY DEFAULT for a mount that arms a live venue account, so a box carrying
/// `flags.poly_reconcile` on and no `VIKE_RECONCILE` reconciles Polymarket where it used to build a client
/// and reconcile nothing — authenticated Polygon-MAINNET reads against a real-money venue with no
/// testnet. That is a real change and it is NAMED rather than smoothed over:
/// `docs/decisions/0043-reconcile-is-on-by-default-under-quarantine.md` carries the verdict and the
/// alternative the owner may prefer, and `docs/ops/reconcile-on-restart.md` tells the operator
/// which single line turns it back off (`flags.poly_reconcile` off, or `VIKE_RECONCILE_OFF=1`).
/// What has NOT changed is that Polymarket still needs an act nobody else needs: every other venue
/// reconciles on the default alone, this one needs `flags.poly_reconcile` on top of it.
///
/// It was feature-gated with the arm in `vike-mount`, whose default build compiled no
/// `("polymarket", _)` arm to call it; here it compiles with the rest of `exec_plane`, under this
/// crate's `polymarket` feature.
#[must_use]
fn poly_recon_wanted(recon_enabled: bool, poly_reconcile: bool) -> bool {
    recon_enabled && poly_reconcile
}

/// The EXEC decision and the real-money factory it guards — the arm's `exec_permitted`
/// conjunction, the one warning it gave, and its call — over the mount's own inputs. A function
/// rather than inline in `PolymarketVenueMount::mount`, with the factory a closure, so the DECISION
/// is testable offline: below `live`, and with `flags.poly_exec` off, the factory — key derivation,
/// the geoblock pre-flight, the L2 round-trip — is never CALLED, which no assertion on the returned
/// `Option` could see.
fn exec_if_permitted(
    inputs: &MountInputs<'_>,
    factory: impl FnOnce() -> Option<PolymarketMount>,
) -> Option<PolymarketMount> {
    // ⚠ THE CEILING AND `flags.poly_exec` ARE BOTH REQUIRED, and neither replaces the other.
    // `venues.polymarket = "live"` does NOT arm exec — the venue's own double gate stands and
    // `flags.poly_exec` is still mandatory. What the ceiling adds is the refusal in the other
    // direction, which is the one that was missing: `paper` overrides `flags.poly_exec` outright,
    // and it does so from ABOVE this mount (`vike-mount` returns the paper engine before it asks any
    // bridge), so `poly_exec_enabled` — which reads the map the daemon folds `flags.poly_exec`
    // into — is never even consulted. A gate that ran after it could be argued with; one that runs
    // before it cannot.
    //
    // `demo` is refused here rather than at that early return, because it is a venue-SPECIFIC fact
    // and this is the venue: Polymarket has NO testnet, so `demo` names a tier that does not exist
    // and the only tier the mount could honour is REAL MONEY on Polygon mainnet. Arming it would be
    // the ceiling widening a mount, which `VenueMode::cap` exists to make impossible.
    //
    // ⚠ RESIDUAL, stated rather than implied: this gates EXEC. Under `demo` the RECON-ONLY lane
    // still runs when `flags.poly_reconcile` is on, exactly as it does today — those are
    // authenticated mainnet READS, they place no order, and narrowing them is a separate decision
    // about what a ceiling governs. Under `paper` nothing in this mount runs at all.
    let exec_flag = poly_exec_enabled(inputs.secrets);
    if exec_flag && !inputs.live_permitted {
        // `paper` never reaches a bridge and `live` permits exec, so the refused ceiling here is
        // always `demo`. It is recorded through `%` (Display), as the arm recorded its `VenueMode`:
        // a bare `&str` field renders QUOTED on the console layer, so `ceiling = "demo"` would
        // change the line.
        tracing::warn!(
            venue = crate::VENUE,
            ceiling = %"demo",
            "flags.poly_exec is on, but this deployment's arming ceiling for polymarket is \
             not `live` → exec stays PAPER. Polymarket has no testnet: `demo` names a tier \
             that does not exist here, so the ceiling refuses rather than arming the only \
             tier there is (real money on Polygon mainnet). Run `vike-cli config set \
             policy.venues.polymarket live` to allow it."
        );
    }
    if exec_flag && inputs.live_permitted { factory() } else { None }
}

/// A mount's outcome from what the exec factory returned — the arm's `match mounted`, pure over its
/// inputs so both halves are testable offline. `recon_only` is the RECON-ONLY reconcile factory,
/// called only when no exec mounted and reconcile was asked for.
///
/// ⚠ `PolymarketMount::tick_regime` is dropped here exactly as the arm dropped it: the contract has
/// no field for it, nothing outside the exec thread ever consumed it, and the exec thread keeps its
/// own clone of the SAME handle, so the self-heal is unaffected.
fn outcome_of(
    mounted: Option<PolymarketMount>,
    want_recon: bool,
    recon_only: impl FnOnce() -> Option<Box<dyn ReconClient>>,
) -> MountOutcome {
    match mounted {
        Some(PolymarketMount { client, recon, tick_regime: _ }) => {
            // LIVE, REAL-MONEY MAINNET exec (the factory already warned, with the funder).
            if want_recon && recon.is_none() {
                tracing::warn!(
                    "polymarket: exec mounted but no reconcile client could be built → \
                     reconcile-inert (exec unaffected)"
                );
            }
            MountOutcome {
                exec: ExecOutcome::Live(LiveExec {
                    client,
                    // The only tier there is. `vike-mount` keys its identity record on it, and this
                    // venue's reconcile client names no account (`fetch_account_identity` keeps the
                    // trait's `Ok(None)`), so nothing is recorded — as under the arm, which handed
                    // the fold `Demo` here (the CEX-only mainnet conjunct) to the same effect.
                    bound_tier: Tier::Live,
                    // No grid of the shape `RiskLimits` reads, so the permissive default stands.
                    grid: None,
                    contract_size: None,
                    // `None` folds to `Cross`, as the arm's `default_margin_mode` literal did,
                    // while `vike_model::caps_for("polymarket").default_margin_mode` is `Cash`.
                    // That is the known sibling `vike-mount`'s `legacy_parts` recorded and left
                    // unfixed on purpose: seeding `Cash` changes what the admitting gate charges
                    // and what `state_hash` covers, which is a venue-verified change of its own,
                    // not a port.
                    margin_mode: None,
                    leg_grids: Vec::new(),
                }),
                recon,
                identity: None,
            }
        }
        None => {
            // Exec off, or it could not be built: absent creds, a bad key, an L2 derivation
            // failure, or a REFUSED region pre-flight (the venue's own geoblock endpoint saying this
            // egress may not place orders — logged at `error!` rather than `warn!`, because that
            // one is a verdict rather than an absence). Each logged its own reason inside the
            // factory. Fall back to the pre-existing RECON-ONLY behavior, which is the paper engine
            // plus, when asked, a reconcile client over its own fresh registry: `ExecOutcome::Paper`
            // with `recon` set, which `vike-mount` folds as a paper engine that reconciles, marks
            // nothing live and records no identity.
            //
            // ⚠ That fallback stays CORRECT under a geoblock refusal, and deliberately so: the
            // venue restricts order PLACEMENT only, so the reads a reconcile client makes are still
            // legal from the refused region.
            let recon = if want_recon {
                let recon = recon_only();
                if recon.is_some() {
                    tracing::warn!(
                        "polymarket: flags.poly_reconcile without a live exec mount → \
                         RECONCILE-ONLY (exec is PAPER). Run under \
                         VIKE_RECONCILE_POLICY=quarantine: an auto-applying policy folds \
                         PositionDrift, which would import the LIVE account's position \
                         into the PAPER engine's books at the venue's avg price. The \
                         startup line from vike_tradehub::reconcile_config names exactly what \
                         the resolved policy auto-applies. See \
                         vike_polymarket::poly_reconcile_enabled"
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
            MountOutcome { exec: ExecOutcome::Paper, recon, identity: None }
        }
    }
}

/// polymarket's VENUE MOUNT. `vike_tradehub::registry::REGISTRY` holds `&PolymarketVenueMount`
/// under that crate's `polymarket` feature; a build without it registers the venue `FeatureAbsent`
/// and mounts it paper.
///
/// ACCOUNT-WIDE, like the recon half: orders, fills, positions and balance all key off the wallet,
/// not the mounted symbol, which is used for exactly one thing — a best-effort pre-resolution of
/// that token's NegRisk signing domain, inert on failure ([`live_mount_for_account`]'s
/// `seed_token`).
pub struct PolymarketVenueMount;

impl VenueMount for PolymarketVenueMount {
    fn venue(&self) -> &'static str {
        crate::VENUE
    }

    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration {
            // `crate::config`'s `load_polymarket_creds_for_account` reads this account's OWN key
            // names; no path reads the default wallet's key for a labelled account.
            addresses_accounts: true,
            process_exclusive: None,
            // Interval-only reconcile: `vike_mount::build_node` threads no reconnect trigger here, and
            // polymarket is not in the node's `recon_feed_statuses`, so its health gate reads
            // Healthy.
            takes_recon_trigger: false,
            // Shares are whole and price is a probability: the venue publishes no instrument grid
            // of the shape `RiskLimits` reads, so the mount fetches none.
            grid_source: DeclaredGridSource::NoGrid,
            // `crate::config`'s `load_polymarket_creds_for_account`: `ADDRESS` is the funder/maker
            // wallet — the thing that HOLDS the outcome tokens — and `PRIVATE_KEY` is the L1 root
            // that derives the L2 trio. No testnet exists, hence the empty demo tier list.
            book_identity: BookIdentity::Named {
                prefix: "POLY",
                demo_tiers: &[],
                live_tiers: &["LIVE"],
                name_suffixes: &["ADDRESS"],
                evm_key_suffixes: &["PRIVATE_KEY"],
            },
            // ⚠ Outcome ④ — the ONE row whose gap is genuinely OURS and genuinely costs orders:
            // `crate::exec_plane::auth`'s `l2_auth_headers` signs `POLY_TIMESTAMP` into every
            // authenticated CLOB request (and `crate::exec_plane::l1`'s `l1_headers` does the same
            // for the key-derivation handshake), so this venue's clock IS on the order path. It is
            // still not read: the CLOB is geo-blocked from the hosts the preflight runs on and is
            // reachable only through the SOCKS egress proxy [`live_mount_for_account`] builds well
            // AFTER that step. The row can only ever print for a mount about to run the LIVE exec
            // client — `vike-mount`'s clock leg lists a venue only when the arming projection arms
            // it, and [`PolymarketVenueMount::resolve`] arms nothing without `flags.poly_exec` and
            // keys — never for a recon-only one. A future lane that moves the proxy earlier should
            // make this `Wired`.
            //
            // ⚠ This row used to read "mounted recon-only behind a feature here" and render N/A,
            // and it could not print in any configuration where that sentence was true: the old
            // text was false exactly when it appeared, and it dressed the roster's one
            // order-affecting gap as "nothing to see here". The row lived in `vike-mount`'s clock
            // table until the venue mount contract moved it here.
            clock: ClockDecl::NotWired {
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
    /// ([`live_mount_for_account`] → the same `load_polymarket_creds_for_account(Live, …)`, `None`
    /// on an unset private key). The flag ALONE is not intent — with no key nothing can ever mount
    /// live, and the pinned contract is that it stays paper and offline. A PRESENT-but-unusable key
    /// IS intent (the same stance as the hyperliquid row): the factory would decline it into paper,
    /// but a key the operator wrote plus the explicit exec flag must refuse over a missing budget,
    /// not silently trade paper.
    ///
    /// ⚠ …plus the ceiling: this venue has no testnet, so its mount refuses anything below `live`
    /// outright, and a `demo`-capped polymarket can only be paper — reporting a session for it
    /// would refuse a mount over a budget it cannot need.
    fn resolve(&self, inputs: &MountInputs<'_>) -> Resolution {
        if !inputs.live_permitted {
            Resolution::Paper(PaperCause::LiveOnlyArm)
        } else if !poly_exec_enabled(inputs.secrets) {
            Resolution::Paper(PaperCause::ExecFlagUnset)
        } else if crate::config::load_polymarket_creds_for_account(
            Environment::Live,
            inputs.account,
            inputs.secrets,
        )
        .is_some()
        {
            Resolution::Armed { tier: Tier::Live, held_below_live: None }
        } else {
            Resolution::Paper(PaperCause::NoCredentials)
        }
    }

    fn mount(&self, req: MountRequest<'_>) -> MountOutcome {
        let vars = req.inputs.secrets;
        let account = req.inputs.account;
        // BOTH gates, and the master one is not decoration here: it is what stops this mount doing
        // the venue's authenticated L2 round trip to build a handle the driver will never be
        // mounted to use. See [`poly_recon_wanted`] for the defect that spelling removes and for
        // what S2 changed for an operator who set `flags.poly_reconcile` and nothing else.
        let want_recon = poly_recon_wanted(req.recon_enabled, poly_reconcile_enabled(vars));
        let mounted = exec_if_permitted(&req.inputs, || {
            // ⚠ `_for_account`: every credential AND the wallet's signature type come from THIS
            // account's key names. On a venue with no testnet that is the whole safety property —
            // a labelled account may never sign with the default wallet's key.
            live_mount_for_account(
                vars,
                account,
                Some(req.symbol),
                want_recon,
                req.events,
                req.inputs.settings,
                req.inputs.process.halt_path.clone(),
            )
        });
        outcome_of(mounted, want_recon, || recon_client_for_account(vars, account))
    }
}

/// Does `s` look like an ERC-1155 outcome `token_id` (a long decimal uint256)? Used only to decide
/// whether the mounted `symbol` is worth a pre-resolution round-trip — the mount passes whatever
/// symbol the node mounted, which for this account-wide venue may be a placeholder.
fn is_token_id(s: &str) -> bool {
    s.len() >= 20 && s.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::assert_matches;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
    }

    /// The venue's MEASURED refusal body, through a German exit (2026-08-23) — the same fixture
    /// `crate::egress`'s parser tests carry, trimmed to the two fields a refusal must name.
    const BLOCKED_DE: &str = r#"{"blocked":true,"country":"DE","region":"SN"}"#;

    /// The exec gate is OFF by default and accepts the EXACT string `"1"` only — the same idiom (and
    /// the same annotated-`.env`-line tolerance) as `poly_reconcile_enabled`.
    #[test]
    fn exec_gate_is_off_by_default_and_exact() {
        assert!(!poly_exec_enabled(&vars(&[])));
        assert!(poly_exec_enabled(&vars(&[(POLY_EXEC_ENV, "1")])));
        assert!(poly_exec_enabled(&vars(&[(POLY_EXEC_ENV, " 1 ")])));
        assert!(poly_exec_enabled(&vars(&[(POLY_EXEC_ENV, "1  # real money, arbdub only")])));
        for off in ["0", "true", "yes", "on", "", "11", "1x"] {
            assert!(!poly_exec_enabled(&vars(&[(POLY_EXEC_ENV, off)])), "{off}");
        }
    }

    /// The venue's settings holding one machine-scoped `venue.polymarket.<field>` row — the shape
    /// the composition root reads out of the settings database (decision 0095).
    fn settings(field: &str, value: &str) -> VenueSettings {
        VenueSettings::from_rows(
            "polymarket",
            &[vike_secrets::VenueSettingRow {
                venue: "polymarket".to_string(),
                tier: None,
                field: field.to_ascii_uppercase(),
                value: value.to_string(),
            }],
        )
    }

    /// The pre-submit-registration gate is OFF by default and accepts the EXACT string `"1"` only —
    /// the same idiom (and annotated-`.env`-line tolerance) as `poly_exec_enabled`, so an unset flag
    /// leaves the exec thread's pre-registration off and behavior byte-identical to before. Read
    /// from the `venue.polymarket.presubmit_register` row (decision 0095, Task 7).
    #[test]
    fn presubmit_register_gate_is_off_by_default_and_exact() {
        let f = "presubmit_register";
        assert!(!presubmit_register_enabled(&VenueSettings::default()));
        assert!(presubmit_register_enabled(&settings(f, "1")));
        assert!(presubmit_register_enabled(&settings(f, " 1 ")));
        // a trailing `.env` comment is tolerated (the `first_token` idiom), same as `poly_exec`
        assert!(presubmit_register_enabled(&settings(f, "1  # derive==orderID live-verified")));
        for off in ["0", "true", "yes", "on", "", "11", "1x"] {
            assert!(!presubmit_register_enabled(&settings(f, off)), "{off}");
        }
        assert!(!presubmit_register_enabled(&settings("rate_gate", "1")), "another field's row");
    }

    /// The escape hatch is OFF by default and accepts the EXACT string `"1"` only — the
    /// `poly_exec_enabled` idiom, including the trailing-`.env`-comment tolerance.
    #[test]
    fn geoblock_override_is_off_by_default_and_exact() {
        let k = POLY_GEOBLOCK_OVERRIDE_ENV;
        assert!(!geoblock_override_enabled(&vars(&[])));
        assert!(geoblock_override_enabled(&vars(&[(k, "1")])));
        assert!(geoblock_override_enabled(&vars(&[(k, " 1 ")])));
        assert!(geoblock_override_enabled(&vars(&[(k, "1  # DE is close-only, we only flatten")])));
        for off in ["0", "true", "yes", "on", "", "11", "1x"] {
            assert!(!geoblock_override_enabled(&vars(&[(k, off)])), "{off}");
        }
    }

    /// A `blocked` verdict REFUSES, and the refusal has to be actionable on its own: the country,
    /// the region, the fact that reads still work, and the way out. An operator reading only this
    /// line must not conclude their credentials or their feeds are broken.
    #[test]
    fn a_blocked_verdict_refuses_and_names_the_country_and_region() {
        let g = crate::egress::parse_geoblock(BLOCKED_DE).unwrap();
        let GeoblockAction::Refuse(m) = geoblock_action(GeoblockVerdict::Blocked(g)) else {
            panic!("a blocked verdict must refuse the exec mount");
        };
        assert!(m.contains("country=DE") && m.contains("region=SN"), "{m}");
        assert!(m.contains("READS") && m.contains("NOT affected"), "{m}");
        assert!(m.contains(POLY_GEOBLOCK_OVERRIDE_ENV), "{m}");
    }

    /// THE OTHER HALF OF THE CONTRACT: a probe that could not be reached or could not be read is a
    /// WARNING, never a refusal. A venue outage, a dead tunnel or an HTML error page must not be
    /// able to fail a mount — that would be a worse defect than the silent 403 this catches.
    #[test]
    fn an_unreachable_or_unreadable_probe_warns_instead_of_refusing() {
        use crate::egress::{geoblock_verdict, parse_geoblock};
        // a network failure, in the shape `observe_geoblock` reports one
        let net = geoblock_verdict(Err("geoblock probe: dial tcp: timed out".to_string()));
        let action = geoblock_action(net);
        assert_matches!(&action, GeoblockAction::Warn(m) if m.contains("timed out"));
        assert!(!matches!(action, GeoblockAction::Refuse(_)));
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
        let overridden = vars(&[(POLY_GEOBLOCK_OVERRIDE_ENV, "1")]);
        let GeoblockAction::Warn(m) = geoblock_preflight(&overridden) else {
            panic!("the override must proceed with a warning, not silently");
        };
        assert!(m.contains("SKIPPED"), "{m}");
        assert!(m.contains(POLY_GEOBLOCK_OVERRIDE_ENV), "{m}");
    }

    /// The `venue.polymarket.exec_markets` row (decision 0095, Task 7): empty is account-wide, and
    /// both separators parse.
    #[test]
    fn markets_default_to_account_wide_and_parse_both_separators() {
        let f = "exec_markets";
        assert!(poly_exec_markets(&VenueSettings::default()).is_empty());
        assert_eq!(poly_exec_markets(&settings(f, "0xaa,0xbb")), vec!["0xaa", "0xbb"]);
        assert_eq!(
            poly_exec_markets(&settings(f, "0xaa 0xbb,  0xcc")),
            vec!["0xaa", "0xbb", "0xcc"]
        );
        // a trailing `.env` comment is annotation, not a market id
        assert_eq!(poly_exec_markets(&settings(f, "0xaa  # the 5m BTC updown")), vec!["0xaa"]);
        assert!(poly_exec_markets(&settings(f, "   ")).is_empty());
    }

    /// Absent / unusable creds ⇒ `None` and **no network call** (the CI-safe half: this test must
    /// return before the `ensure_l2` round-trip, exactly like `recon_client_from_vars`'s twin).
    #[test]
    fn without_a_usable_key_the_mount_is_none_and_offline() {
        let (tx, _rx) = vike_exec::event_channel(8);
        let none = VenueSettings::default();
        // No client is ever built on these paths, so the sentinel is never looked at.
        let no_halt = std::path::PathBuf::new;
        assert!(live_mount_from_vars(&vars(&[]), None, true, &tx, &none, no_halt()).is_none());
        assert!(
            live_mount_from_vars(
                &vars(&[("POLY_FUNDER", "0xabc")]),
                None,
                true,
                &tx,
                &none,
                no_halt()
            )
            .is_none()
        );
        assert!(
            live_mount_from_vars(
                &vars(&[("POLY_PRIVATE_KEY", "not-a-key")]),
                None,
                true,
                &tx,
                &none,
                no_halt()
            )
            .is_none()
        );
    }

    #[test]
    fn token_id_shape_gate() {
        assert!(is_token_id(&"7".repeat(70)));
        assert!(!is_token_id("BTCUSDT"));
        assert!(!is_token_id("123")); // too short to be a uint256 token id
        assert!(!is_token_id(&format!("0x{}", "a".repeat(64)))); // a condition id, not a token id
    }
}

#[path = "mount_contract_tests.rs"]
#[cfg(test)]
mod mount_contract_tests;
