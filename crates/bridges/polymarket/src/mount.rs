//! The Polymarket LIVE-MOUNT factory — the one seam `vike_mount::make_engine`'s `("polymarket", _)`
//! arm calls, so no signer / derivation / address / websocket detail lives in the composition root
//! (the same discipline [`crate::recon_client::recon_client_from_vars`] follows for the recon half).
//!
//! [`live_mount_from_vars`] returns the exec client AND the recon client **built over ONE shared
//! [`PolymarketRegistry`] and ONE L2 handshake**. That sharing is the point: the CLOB assigns order
//! ids and never echoes a client id, so a reconcile report can only name a local coid if it reads
//! the very map the exec thread writes. Before this existed, `make_engine` built the recon client
//! with a FRESH empty registry and every order report came back `client_order_id: None`.
//!
//! ## What is gated, and why there are two gates
//! - `POLY_EXEC=1` ([`poly_exec_enabled`]) — the exec gate. Polymarket has **no testnet**: every
//!   order this mounts is real money on Polygon mainnet, so the venue is opt-in twice over (creds
//!   AND this flag) rather than the usual once. Absent ⇒ `make_engine` builds nothing here, makes no
//!   network call, and the venue stays PAPER — byte-identical to before this existed.
//! - `POLY_RECONCILE=1` ([`crate::recon_client::poly_reconcile_enabled`]) — unchanged, still gates
//!   the reconcile half independently. The two compose: exec-only, recon-only, both, or neither.
//!
//! A THIRD flag, `POLY_PRESUBMIT_REGISTER=1` ([`presubmit_register_enabled`]), is NOT a mount gate
//! but a within-exec behavior toggle threaded onto [`PolymarketLiveConfig::presubmit_register`]: ON,
//! the exec thread pre-registers `coid`↔`derive_order_id(&order)` just before each submit to close
//! the ack-race (a user-WS fill can beat the HTTP ack). Default OFF ⇒ byte-identical to today.
//!
//! All three read the process env FIRST and the workspace `.env` map second, the idiom
//! [`crate::egress::proxy_url`] established (a flag that is silently ignored in the file where every
//! other Polymarket setting lives is a bug, not a convenience).
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
//! A FOURTH flag, `POLY_GEOBLOCK_OVERRIDE=1` ([`geoblock_override_enabled`]), skips it. Unlike the
//! three above it is read from the caller-supplied map ONLY, never the process env; that is a
//! settings-registry constraint rather than a preference, and its doc carries the argument.
//!
//! The **dynamic tick-size regime** ([`crate::tick_regime`]) is deliberately NOT a flag at all: this
//! factory always builds one and threads it into the exec thread, because it exists to repair a live
//! failure the venue reports only as an endless stream of rejects, and an operator cannot know to
//! turn it on before hitting that. Mounting it is safe to do unconditionally because it starts EMPTY
//! and an unresolved token prices verbatim, so no price moves until an off-grid reject has actually
//! taught it that token's grid. It rides `POLY_EXEC=1` — an exec-less mount builds nothing here.

use std::collections::HashMap;

use vike_bridge_core::credentials::Environment;
use vike_exec::recon::ReconClient;
use vike_exec::{EventSender, ExecutionClient};

use crate::client::{
    decode_builder_bytes32, PolymarketExecutionClient, PolymarketLiveConfig, UserChannelConfig,
};
use crate::config::{first_token, PolymarketCreds, CLOB_BASE, WS_USER};
use crate::egress::{check_order_placement_geo, GeoblockVerdict};
use crate::neg_risk_lookup::NegRiskSource;
use crate::recon_client::{recon_client, signature_type_for_account};
use crate::registry::PolymarketRegistry;
use crate::tick_regime::TickRegime;

/// The workspace-`.env` / process-env name of the Polymarket **exec** opt-in.
pub const POLY_EXEC_ENV: &str = "POLY_EXEC";
/// The workspace-`.env` / process-env name of the user-channel market list (see
/// [`poly_exec_markets`]).
pub const POLY_EXEC_MARKETS_ENV: &str = "POLY_EXEC_MARKETS";
/// The workspace-`.env` / process-env name of the pre-submit-registration opt-in (see
/// [`presubmit_register_enabled`]).
pub const POLY_PRESUBMIT_REGISTER_ENV: &str = "POLY_PRESUBMIT_REGISTER";
/// The credential-store name of the geoblock pre-flight's escape hatch (see
/// [`geoblock_override_enabled`]).
pub const POLY_GEOBLOCK_OVERRIDE_ENV: &str = "POLY_GEOBLOCK_OVERRIDE";

/// The exec opt-in: the EXACT string `"1"` (the `VIKE_RECONCILE` idiom — not a fuzzy truthy parse),
/// in the process env OR the workspace `.env` map. Default OFF.
///
/// This is a SECOND gate on top of absent-credentials-is-the-live-gate, and it exists because
/// Polymarket is the one mounted venue with no demo tier at all: on every other venue a mis-set
/// credential mounts a *demo* client, here it mounts a real one. `POLY_EXEC` unset means an operator
/// who merely has keys in their `.env` — which the reconcile half, the data feeds and the redeem
/// poller all legitimately want — cannot accidentally arm order placement.
pub fn poly_exec_enabled(vars: &HashMap<String, String>) -> bool {
    std::env::var(POLY_EXEC_ENV).as_deref() == Ok("1")
        || vars.get(POLY_EXEC_ENV).map(|v| first_token(v)) == Some("1")
}

/// The pre-submit-registration opt-in: the EXACT string `"1"` (the `POLY_EXEC` / `VIKE_RECONCILE`
/// idiom — not a fuzzy truthy parse), in the process env OR the workspace `.env` map. Default OFF.
///
/// ON ⇒ the exec thread pre-registers `coid`↔`crate::order::derive_order_id(&order)` in the shared
/// registry just BEFORE each network submit, closing the ack-race in which a user-WS fill/cancel
/// beats the HTTP acceptance and lands keyed by an order id the registry does not yet hold (the
/// `client_order_id: None` path). Safe because that derived id is the CLOB's own `orderID` — the
/// venue keys orders by the EIP-712 order hash — and the later real `OrderAccepted` re-registers the
/// SAME `(coid, id, side)` as an idempotent no-op (see [`PolymarketRegistry::on_accept`]).
///
/// Kept a SEPARATE gate from `POLY_EXEC` (rather than always-on with exec) as verify-before-enable
/// discipline: the `derive_order_id == orderID` premise is LIVE-VERIFIED on a real demo order
/// (`crate::client::tests::live_place_and_cancel`), but booking a pre-registered id into the money
/// path is opt-in until that smoke is re-run against the deployment. Unset (the default) ⇒ nothing
/// is registered pre-submit — byte-identical to before this existed.
///
/// ⚠ **This flag is no longer the only thing standing between a raced fill and a silent loss, and
/// must not be read as such.** It ships OFF, and while it was the sole mitigation an executed fill
/// that beat the ack was simply discarded at `crate::user_ws`'s re-key sites — position and PnL
/// silently wrong. The unconditional staging in [`crate::pending_events`] now covers that race in
/// every build, flag or no flag. What the flag still buys is AVOIDING the stage entirely (the id is
/// already there when the frame lands), which is cheaper and needs no TTL to be right — but it is an
/// optimisation over the default now, not the safety net.
pub fn presubmit_register_enabled(vars: &HashMap<String, String>) -> bool {
    std::env::var(POLY_PRESUBMIT_REGISTER_ENV).as_deref() == Ok("1")
        || vars.get(POLY_PRESUBMIT_REGISTER_ENV).map(|v| first_token(v)) == Some("1")
}

/// The CLOB **condition ids** the user-channel subscribe frame names, from `POLY_EXEC_MARKETS`
/// (comma- and/or whitespace-separated; process env first, workspace `.env` second).
///
/// ⚠ These are market `condition_id`s, NOT outcome `token_id`s — the two look alike (both long hex/
/// decimal strings) and the user channel silently streams nothing if you hand it token ids. The
/// `markets` field of the subscribe frame is the same one
/// [`user_subscribe_message`](crate::user_ws::user_subscribe_message) takes and the
/// `polymarket_user_smoke` passes a `condition_id` to.
///
/// An EMPTY list (the default) subscribes account-wide — the CLOB user channel treats an empty
/// `markets` array as "every market this key trades", which is what a mount wants when the strategy
/// picks its markets at runtime. That behavior is asserted live by
/// `poly_exec_mount_smoke::empty_markets_is_account_wide`, because it is a server-side convention
/// with no contractual documentation; if a future deployment changes it, set `POLY_EXEC_MARKETS`
/// explicitly and the pump narrows to exactly those markets. **Either way the A3 resync is the
/// backstop**: it replays `/data/trades` + `/data/orders`, which are account-wide regardless of the
/// subscribe list, after every reconnect.
pub fn poly_exec_markets(vars: &HashMap<String, String>) -> Vec<String> {
    let raw = std::env::var(POLY_EXEC_MARKETS_ENV)
        .ok()
        .or_else(|| vars.get(POLY_EXEC_MARKETS_ENV).cloned())
        .unwrap_or_default();
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
/// ⚠ **Map-ONLY, unlike its three siblings above, and that asymmetry is deliberate.**
/// [`poly_exec_enabled`], [`presubmit_register_enabled`] and [`poly_exec_markets`] read the process
/// env first and the map second; each of those reads is a `Layer::Library` row on the settings
/// registry's RATCHET, which may shrink and never grow —
/// `crates/vike-ops/tests/settings_registry.rs`'s `library_rows_do_not_grow` — so a fourth process-
/// env read here would redden CI. Taking the value as a parameter is the repo's stated target shape
/// anyway (`vike_ops::settings`' `Layer::Injected`), so this hatch is set where the venue's other
/// credentials already live, `<project>/settings/secrets.env`, and the refusal message says so.
///
/// It ARMS nothing on its own: `POLY_EXEC=1` is still required, is still refused in that same file
/// by `vike_config::refuse_credential_file_arming`, and is still a process-env fact. All this flag
/// can do is decline a pre-flight — the orders it lets through are the ones the operator had
/// already armed, and a genuinely blocked region still refuses each of them at submit.
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
             region (POLY_SOCKS_PROXY / POLY_PROXY_HOST / POLY_PROXY_PORT), or set \
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
    /// The engine `make_engine` boxes as its `Box<dyn ExecutionClient>`.
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
/// Returns `None` — leaving `make_engine` on its paper fallback — for every one of:
/// absent `POLY_PRIVATE_KEY` (absent-credentials-is-the-live-gate), an unusable key, a REFUSED
/// region pre-flight (the venue itself says this egress may not place orders — [`geoblock_action`],
/// logged at `error!` rather than `warn!` because it is a verdict rather than an absence), or a
/// failed L2 derivation (tunnel down / geo-blocked / rejected ClobAuth signature). None of those is
/// a mount failure or a panic; each logs its own reason. The gates themselves (`POLY_EXEC` /
/// `POLY_RECONCILE`) are the CALLER's to check — `make_engine` checks them so that an unset flag
/// makes no network call at all.
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
/// `seed_token` is an optional outcome `token_id` (typically `make_engine`'s mounted `symbol`) whose
/// NegRisk flag is pre-resolved here so the FIRST order pays no lookup latency; a failure to resolve
/// it is inert — the exec thread's lazy [`NegRiskSource::lookup`] retries per order.
pub fn live_mount_from_vars(
    vars: &HashMap<String, String>,
    seed_token: Option<&str>,
    want_recon: bool,
    events: &EventSender,
) -> Option<PolymarketMount> {
    live_mount_for_account(
        vars,
        &vike_model::account_keys::AccountLabel::Default,
        seed_token,
        want_recon,
        events,
    )
}

/// [`live_mount_from_vars`] for ONE NAMED ACCOUNT — everything it documents, with every credential
/// and the signature type read through THIS account's key names
/// (`POLY_{TIER}_{SUFFIX}__{LABEL}`, `POLY_SIGNATURE_TYPE__{LABEL}`).
///
/// ⚠ **[`AccountLabel::Default`](vike_model::account_keys::AccountLabel::Default) is byte-identically
/// [`live_mount_from_vars`]**, reached through it — the grammar returns every key name unchanged for
/// that account.
///
/// ⚠ **No path here reads the default account's key on behalf of a labelled one.** That is what the
/// refusal in `vike_mount::arm_addresses_accounts` used to stand in for: on a REAL-MONEY-only venue,
/// a second engine built on the first wallet's key is two engines trading one account.
///
/// ⚠ What stays DEPLOYMENT-wide, deliberately: the `POLY_EXEC` gate (the caller's to check), the
/// `POLY_USER_MARKETS` subscribe list, `POLY_PRESUBMIT_REGISTER`, the geoblock pre-flight and the
/// builder attribution code. None of them names a wallet — they describe this process's egress and
/// behaviour, so a second account inherits them by being on the same box.
pub fn live_mount_for_account(
    vars: &HashMap<String, String>,
    label: &vike_model::account_keys::AccountLabel,
    seed_token: Option<&str>,
    want_recon: bool,
    events: &EventSender,
) -> Option<PolymarketMount> {
    // Polymarket is Polygon-MAINNET-only — there is no testnet — so the live tier is the only tier.
    let loaded = crate::config::load_polymarket_creds_for_account(Environment::Live, label, vars)?;
    let signer = match vike_bridge_core::eip712::eth_address_from_private_key(&loaded.private_key) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(target: "vike_polymarket::mount", error = %e, "polymarket exec unmounted: POLY_PRIVATE_KEY is not a usable key");
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
        GeoblockAction::Warn(m) => tracing::warn!(target: "vike_polymarket::mount", "{m}"),
        GeoblockAction::Refuse(m) => {
            tracing::error!(target: "vike_polymarket::mount", "{m}");
            return None;
        }
    }
    let funder = if loaded.address.is_empty() { signer.clone() } else { loaded.address.clone() };
    let mut creds = PolymarketCreds { address: signer, ..loaded };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    if let Err(e) = crate::l1::ensure_l2(&mut creds, CLOB_BASE, now) {
        tracing::warn!(target: "vike_polymarket::mount", error = %e, "polymarket exec unmounted: L2 derivation failed (proxy down / geo-blocked?)");
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
    if let Some(tok) = seed_token.filter(|s| is_token_id(s)) {
        if let Some(nr) = crate::neg_risk_lookup::clob_neg_risk_fetch()(tok) {
            seed.insert(tok.to_string(), nr);
        }
    }

    let markets = poly_exec_markets(vars);
    // Fee-attribution builderCode, resolved ONCE from the workspace `.env`: `attribution_code_from`
    // validates it against the venue's `AttributionMechanic`, then `decode_builder_bytes32` turns the
    // 0x-hex string into the signed field's raw bytes32. Absent/malformed degrades to `[0u8; 32]`
    // (unattributed) rather than failing the mount — this venue has no other place to surface that.
    let builder_code = vike_bridge_core::credentials::attribution_code_from(vars, "polymarket")
        .and_then(decode_builder_bytes32)
        .unwrap_or([0u8; 32]);
    tracing::warn!(
        target: "vike_polymarket::mount",
        %funder,
        sig_type = signature_type.code(),
        user_markets = markets.len(),
        "polymarket: POLY_EXEC=1 + creds → LIVE MAINNET exec mount (REAL MONEY — no testnet exists)"
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
                history_limit: crate::client::DEFAULT_HISTORY_LIMIT,
            }),
            builder_code,
            // Ack-race close (`POLY_PRESUBMIT_REGISTER`, default OFF): pre-register coid↔derived-id
            // before each submit so a user-WS fill can't outrun the registry. OFF ⇒ byte-identical.
            presubmit_register: presubmit_register_enabled(vars),
            // The dynamic tick-size regime: the exec thread's clone of the cache above. It prices
            // each submit on this token's known grid and re-fetches that grid on an off-grid reject,
            // so a maker whose tick moved under it is corrected instead of refused forever.
            tick_regime: Some(tick_regime.clone()),
        },
        events.clone(),
    );
    // The recon client over the SAME derived creds and the SAME registry the exec thread writes —
    // the whole reason this factory exists rather than two independent ones.
    let recon =
        if want_recon { recon_client(creds, funder, signature_type, registry) } else { None };
    Some(PolymarketMount { client: Box::new(client), recon, tick_regime })
}

/// Does `s` look like an ERC-1155 outcome `token_id` (a long decimal uint256)? Used only to decide
/// whether the mounted `symbol` is worth a pre-resolution round-trip — `make_engine` passes whatever
/// symbol the app mounted, which for this account-wide venue may be a placeholder.
fn is_token_id(s: &str) -> bool {
    s.len() >= 20 && s.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// The pre-submit-registration gate is OFF by default and accepts the EXACT string `"1"` only —
    /// the same idiom (and annotated-`.env`-line tolerance) as `poly_exec_enabled`, so an unset flag
    /// leaves the exec thread's pre-registration off and behavior byte-identical to before.
    #[test]
    fn presubmit_register_gate_is_off_by_default_and_exact() {
        let k = POLY_PRESUBMIT_REGISTER_ENV;
        assert!(!presubmit_register_enabled(&vars(&[])));
        assert!(presubmit_register_enabled(&vars(&[(k, "1")])));
        assert!(presubmit_register_enabled(&vars(&[(k, " 1 ")])));
        // a trailing `.env` comment is tolerated (the `first_token` idiom), same as `poly_exec`
        assert!(presubmit_register_enabled(&vars(&[(k, "1  # derive==orderID live-verified")])));
        for off in ["0", "true", "yes", "on", "", "11", "1x"] {
            assert!(!presubmit_register_enabled(&vars(&[(k, off)])), "{off}");
        }
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
        assert!(matches!(&action, GeoblockAction::Warn(m) if m.contains("timed out")));
        assert!(!matches!(action, GeoblockAction::Refuse(_)));
        // an unparseable body (a proxy's error page, say) takes the same road
        let junk = geoblock_verdict(parse_geoblock("<html>502 Bad Gateway</html>"));
        assert!(matches!(geoblock_action(junk), GeoblockAction::Warn(_)));
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

    #[test]
    fn markets_default_to_account_wide_and_parse_both_separators() {
        assert!(poly_exec_markets(&vars(&[])).is_empty());
        assert_eq!(
            poly_exec_markets(&vars(&[(POLY_EXEC_MARKETS_ENV, "0xaa,0xbb")])),
            vec!["0xaa", "0xbb"]
        );
        assert_eq!(
            poly_exec_markets(&vars(&[(POLY_EXEC_MARKETS_ENV, "0xaa 0xbb,  0xcc")])),
            vec!["0xaa", "0xbb", "0xcc"]
        );
        // a trailing `.env` comment is annotation, not a market id
        assert_eq!(
            poly_exec_markets(&vars(&[(POLY_EXEC_MARKETS_ENV, "0xaa  # the 5m BTC updown")])),
            vec!["0xaa"]
        );
        assert!(poly_exec_markets(&vars(&[(POLY_EXEC_MARKETS_ENV, "   ")])).is_empty());
    }

    /// Absent / unusable creds ⇒ `None` and **no network call** (the CI-safe half: this test must
    /// return before the `ensure_l2` round-trip, exactly like `recon_client_from_vars`'s twin).
    #[test]
    fn without_a_usable_key_the_mount_is_none_and_offline() {
        let (tx, _rx) = vike_exec::event_channel(8);
        assert!(live_mount_from_vars(&vars(&[]), None, true, &tx).is_none());
        assert!(live_mount_from_vars(&vars(&[("POLY_FUNDER", "0xabc")]), None, true, &tx).is_none());
        assert!(live_mount_from_vars(&vars(&[("POLY_PRIVATE_KEY", "not-a-key")]), None, true, &tx)
            .is_none());
    }

    #[test]
    fn token_id_shape_gate() {
        assert!(is_token_id(&"7".repeat(70)));
        assert!(!is_token_id("BTCUSDT"));
        assert!(!is_token_id("123")); // too short to be a uint256 token id
        assert!(!is_token_id(&format!("0x{}", "a".repeat(64)))); // a condition id, not a token id
    }
}
