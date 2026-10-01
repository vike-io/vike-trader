//! binance's VENUE MOUNT — the `vike_bridge_core::venue_mount::VenueMount` contract over the spot
//! and USDⓈ-M perp exec client, its reconcile client and the withdraw-permission gate that guards
//! them (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md).
//!
//! Moved, behaviour unchanged, from `vike-mount`: the `("binance", Some(c))` arm and the pre-match
//! withdraw gate that guarded it (now `mount_with`, which [`BinanceVenueMount::mount`] calls, and
//! `binance_withdraw_gate`), its row of the legacy arming probe ([`BinanceVenueMount::resolve`]),
//! its clock, book-identity and grid-source rows ([`BinanceVenueMount::declaration`]), its clock
//! read and parse ([`BinanceVenueMount::server_time_ms`]), its `AUTHED_READ_MARKETS` startup probe
//! ([`BinanceVenueMount::credential_probe`]), its arm of the `build_recon_client` dispatch and its
//! fallback grid.
//!
//! # The ceiling decides the network (D1)
//!
//! `live` means mainnet (docs/decisions/0095-venues-read-no-environment-and-live-means-mainnet.md):
//! a `live` ceiling reads the LIVE key set and binds the mainnet hosts, every lower ceiling reads
//! DEMO and binds the demo hosts, and there is no switch beside the ceiling. So
//! [`MountInputs::live_permitted`] is the whole tier answer — for the credentials, the withdraw
//! gate, the grid pre-fetch, the exec hosts, the reconcile client, the clock read and the tier
//! handed to the identity record (inert for binance today — see `bound_tier`) alike — and a `live`
//! ceiling with no LIVE key set stays PAPER: a mainnet host is never signed with demo keys.
//!
//! # `.P` routing
//!
//! One venue id, two exec lanes: a trailing `.P` on the mounted symbol selects the USDⓈ-M perp,
//! anything else spot. Nothing here routes. `crate::exec`'s `fetch_binance_properties` and
//! `BinanceExecutionClient` and `crate::recon_client`'s `recon_client` each split the suffix
//! themselves, the same way, so one mount's grid, exec client and reconcile client cannot land on
//! different lanes.
//!
//! # The withdraw gate
//!
//! `binance_withdraw_gate` is this mount's one decline after `resolve` armed: it needs a signed
//! network read, so it cannot live in the pure probe. It runs before anything is built — the grid
//! pre-fetch, the reconcile client and the exec spawn all come after it — which is where the legacy
//! arm's guard put it, and `vike-mount`'s pre-connect budget refusal has already run by the time
//! `mount` is called, as it had before that guard. The override is `flags.allow_withdraw_keys`,
//! which `vike-tradehub` folds into the map it hands the mount
//! (`crates/vike-tradehub/src/tradehub_cli.rs`'s `fold_flags_into_vars`, which OVERWRITES whatever
//! the credential store carried under that name), so [`MountInputs::secrets`] is its one source —
//! the source the legacy gate read, alone, since decision 0095 retired the variable. No process
//! environment is consulted.
//!
//! # The network steps are parameters
//!
//! `mount_with` is the mount's body with its three NETWORK steps — the withdraw gate's signed
//! permission read, the grid pre-fetch and the exec spawn whose thread dials the venue — taken as
//! parameters. [`BinanceVenueMount::mount`] passes the real three; this module's tests pass
//! offline doubles, so they read what the body binds, spawns and declines rather than what a helper
//! beside it answers. Nothing else in the body reaches a socket: `crate::recon_client`'s
//! `recon_client` only constructs.

use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::Duration;

use vike_bridge_core::credentials::{
    Credentials, Environment, attribution_code_from, load_credentials_for_account,
    load_credentials_from,
};
use vike_bridge_core::key_permissions::{
    KeyPermissionProbe, KeyPermissions, WithdrawGate, allow_withdraw_keys, withdraw_gate,
};
use vike_bridge_core::venue_mount::{
    BookIdentity, ClockAuth, ClockDecl, ClockRisk, CredentialProbe, DeclaredGridSource,
    ExecOutcome, LiveExec, MountInputs, MountOutcome, MountRequest, PaperCause, Resolution, Tier,
    VenueDeclaration, VenueMount, bounded_public_get, missing_time_field,
};
use vike_exec::{EventSender, ExecutionClient};
use vike_model::SymbolProperties;

use crate::VENUE;

/// The symbol the startup credential probe scopes its reconcile client to — `vike-mount`'s
/// `AUTHED_READ_MARKETS` row for this venue, which mirrored the wired binance market (`crates/vike-tradehub/src/wired_markets.rs`'s `BINANCE_MARKET` since docs/decisions/0098). It
/// scopes only the client's order and position reads; the balance read the probe issues is
/// account-wide.
const PROBE_SYMBOL: &str = "BTCUSDT";

/// What a `live` ceiling with no LIVE key set is told — the line `vike-mount`'s legacy prefix
/// logged at its `MainnetNoCreds` site, minus the `{venue}: ` it interpolated (a `const` cannot).
const NO_LIVE_CREDENTIALS: &str = "the ceiling is `live` (MAINNET) but no LIVE credentials are \
     present → staying PAPER (a mainnet host is never signed with demo keys; absent credentials \
     are the live gate)";

/// FALLBACK Binance BTCUSDT grid, used ONLY if the exec thread's startup `exchangeInfo` fetch
/// fails. Spot-ish BTC values (the `.P` perp grid is coarser, but this is inert once the real grid
/// loads via `crate::exec`'s `fetch_binance_properties`). Moved from `vike-mount`'s `fallback.rs`.
fn fallback_properties() -> SymbolProperties {
    SymbolProperties {
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

/// `{"serverTime": <epoch ms>}` — the shape `/api/v3/time` answers. Split from the fetch so the
/// field name is gated by a fixture test rather than by a live call. The parse `vike-mount`'s
/// legacy clock row applied to this venue (`parse_binance_shaped_time`).
fn parse_server_time(body: &serde_json::Value) -> Result<i64, String> {
    body.get("serverTime")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| missing_time_field("serverTime"))
}

/// The contract's tier for a resolved mainnet verdict — the tier the credentials authenticate, and
/// the tier `vike-mount` hands to its identity record. That record is inert for binance today:
/// `crate::recon_client`'s `BinanceReconClient` does not forward `fetch_account_identity`, so the
/// trait's default answers `Ok(None)` and nothing is recorded.
fn bound_tier(mainnet: bool) -> Tier {
    if mainnet { Tier::Live } else { Tier::Demo }
}

/// The withdraw override: `flags.allow_withdraw_keys`, as the exact string `"1"` under
/// `VIKE_ALLOW_WITHDRAW_KEYS` in the map the composition root folded the RESOLVED flag into (module
/// doc) — the map `vike-mount`'s legacy gate read, the same way.
fn withdraw_override(inputs: &MountInputs<'_>) -> bool {
    allow_withdraw_keys(inputs.secrets)
}

/// The pure verdict core of `binance_withdraw_gate`: a fetched permission set folds through
/// `vike_bridge_core::key_permissions::withdraw_gate`, and a fetch ERROR is Unknown, which never
/// refuses.
fn binance_withdraw_verdict(
    fetched: Result<KeyPermissions, String>,
    allow_override: bool,
) -> WithdrawGate {
    let perms = fetched.unwrap_or(KeyPermissions::UNKNOWN);
    withdraw_gate(&perms, allow_override)
}

/// **May a key that can WITHDRAW arm a live venue?** — STEP 2 of the api-key-permissions
/// capability map (`vike_bridge_core::key_permissions`'s module doc is the authority on the
/// policy). Moved from `vike-mount`, whose legacy arm consulted it before its `match`.
///
/// - `mainnet == false` ⇒ `Allow` with **NO network call at all**: `fetch_permissions` is never
///   called. The probe reads `/sapi/v1/account/apiRestrictions`, and `sapi` is MAINNET-ONLY — the
///   demo host does not serve it, so a demo mount could only produce a doomed round trip whose
///   failure maps to Unknown ⇒ Allow anyway.
/// - A FETCH ERROR is Unknown, which ALLOWS (fail-open on introspection): we refuse on evidence,
///   never on the absence of evidence.
/// - A KNOWN withdraw-capable key REFUSES and the mount stays paper — the same outcome absent
///   credentials produce — unless `allow_override`.
///
/// `fetch_permissions` is the signed read itself ([`BinanceVenueMount::mount`] passes
/// `crate::key_permissions`'s `key_permission_probe` against `crate::spot::MAINNET_REST`), a
/// parameter so a test can see whether it is issued.
///
/// Blocking: at most ONE signed GET, and only for a live MAINNET binance mount — the exact mount
/// whose first order would otherwise be the probe.
fn binance_withdraw_gate(
    mainnet: bool,
    creds: &Credentials,
    allow_override: bool,
    fetch_permissions: impl FnOnce(&Credentials) -> Result<KeyPermissions, String>,
) -> WithdrawGate {
    if !mainnet {
        return WithdrawGate::Allow;
    }
    let fetched = fetch_permissions(creds);
    if let Err(e) = &fetched {
        tracing::warn!(
            venue = VENUE,
            error = %e,
            "key-permission introspection failed → permissions Unknown (fail-open); the withdraw \
             gate refuses only a KNOWN withdraw-capable key"
        );
    }
    let verdict = binance_withdraw_verdict(fetched, allow_override);
    if verdict == WithdrawGate::Refuse {
        tracing::error!(
            venue = VENUE,
            "⚠ REFUSING to arm binance LIVE: this API key can WITHDRAW. Falling back to the PAPER \
             client (the same outcome absent credentials produce). Re-issue a trade-only key, or \
             `vike-cli config set flags.allow_withdraw_keys true` and restart to override."
        );
    }
    verdict
}

/// `crate::exec`'s `BinanceExecutionClient::spawn_with_recorder` arguments, in its order, as
/// `mount_with` assembles them — the one value its spawn step is handed.
struct ExecSpawn {
    env: Environment,
    creds: Credentials,
    symbol: String,
    fallback: SymbolProperties,
    events: EventSender,
    properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
    on_reconcile: Option<Sender<()>>,
    link_id: Option<String>,
    leverage: f64,
    trade_lite_fill: bool,
    /// The HALT sentinel the client watches — `MountInputs::process.halt_path`, handed to the
    /// client by its `with_halt_path` (decision 0099). Not an argument of `spawn_with_recorder`:
    /// it rides the builder, applied by `mount`'s spawn step.
    halt_path: std::path::PathBuf,
}

/// [`BinanceVenueMount::mount`]'s body, with its three NETWORK steps taken as parameters (module
/// doc): `fetch_permissions` is the withdraw gate's signed read, `fetch_grid` the grid pre-fetch
/// and `spawn` the exec client whose thread dials the venue.
fn mount_with(
    req: MountRequest<'_>,
    fetch_permissions: impl FnOnce(&Credentials) -> Result<KeyPermissions, String>,
    fetch_grid: impl FnOnce(Environment, &Credentials, &str) -> Option<SymbolProperties>,
    spawn: impl FnOnce(ExecSpawn) -> Box<dyn ExecutionClient + Send>,
) -> MountOutcome {
    let Some(c) = BinanceVenueMount::credentials(&req.inputs) else {
        if req.inputs.live_permitted {
            tracing::warn!(venue = VENUE, "{VENUE}: {NO_LIVE_CREDENTIALS}");
        }
        return MountOutcome::paper();
    };
    // Decision 0095: for this venue the ceiling IS the network.
    let mainnet = req.inputs.live_permitted;
    // THE WITHDRAW GATE, before anything is built: one signed read on mainnet, none on demo. A
    // `Refuse` is the paper outcome — no grid fetched, no reconcile client, nothing spawned — as
    // the legacy guard's failing sent the venue to the paper arm.
    if binance_withdraw_gate(mainnet, &c, withdraw_override(&req.inputs), fetch_permissions)
        == WithdrawGate::Refuse
    {
        return MountOutcome::paper();
    }
    // The hosts bind through the adapter's own pure `resolve_env_from` core, so the demo→mainnet
    // upgrade rule lives in exactly one place; `mainnet == false` ⇒ `Demo`.
    let env = crate::exec::resolve_env_from(Environment::Demo, mainnet);
    if mainnet {
        tracing::warn!(
            "⚠ REAL-MONEY: binance mounting on MAINNET with LIVE credentials (real funds)"
        );
    } else {
        tracing::warn!("binance: DEMO credentials present → LIVE exec client (real demo orders)");
    }
    // Live RiskGate from the venue's REAL grid (PR-2b): one blocking pre-fetch (spot /api/v3 or
    // fapi /fapi/v1 exchangeInfo, routed inside by the `.P` suffix — a duplicate of the exec
    // thread's own, acceptable startup-only). On failure the permissive default stands.
    let grid = fetch_grid(env, &c, req.symbol);
    // Reconcile handle: a FRESH HMAC REST client dedicated to reconcile reads, built from `&c`
    // before `c` moves into the spawn, on the SAME `.P` lane and the SAME resolved `mainnet` the
    // exec client binds, so it reconciles the account these credentials authenticate. Pure to
    // construct, so NOT gated on `recon_enabled` — exactly as the legacy arm built it.
    let recon = crate::recon_client::recon_client(&c, req.symbol, mainnet);
    // Fee-attribution Broker/Link id (`BINANCE_BROKER_CODE`/`BINANCE_BUILDER_CODE`), validated
    // against binance's `AttributionMechanic`. Absent/invalid degrades to `None` — the wire body's
    // `newClientOrderId`/`origClientOrderId` then carry the bare local coid.
    let link_id = attribution_code_from(req.inputs.secrets, VENUE);
    // ACCOUNT LEVERAGE from the operator's `[risk]` budget; unset ⇒ the historical 2.0. Inert on a
    // spot symbol. UNCLAMPED, unlike bybit/okx — `crate::exec`'s `leverage_for` carries why.
    let leverage = crate::exec::leverage_for(req.risk_profile);
    // The TRADE_LITE early-fill hint — `venue.binance.trade_lite_fill` (decision 0095), read once
    // from the venue's settings.
    let trade_lite_fill = crate::perp_user_data::trade_lite_fill(req.inputs.settings);
    MountOutcome {
        exec: ExecOutcome::Live(LiveExec {
            client: spawn(ExecSpawn {
                env,
                creds: c,
                symbol: req.symbol.to_string(),
                fallback: fallback_properties(),
                events: req.events.clone(),
                properties_rec: req.properties_rec,
                on_reconcile: req.recon_trigger,
                link_id,
                leverage,
                trade_lite_fill,
                halt_path: req.inputs.process.halt_path.clone(),
            }),
            bound_tier: bound_tier(mainnet),
            grid,
            contract_size: None,
            margin_mode: None,
            leg_grids: Vec::new(),
        }),
        recon,
        identity: None,
    }
}

/// binance's mount. `vike_tradehub::registry::REGISTRY` holds `&BinanceVenueMount`.
pub struct BinanceVenueMount;

impl BinanceVenueMount {
    /// The key tier the ceiling selects (D1): LIVE under a `live` ceiling, DEMO otherwise.
    fn tier(inputs: &MountInputs<'_>) -> Environment {
        if inputs.live_permitted { Environment::Live } else { Environment::Demo }
    }

    /// The one gate `resolve` and `mount` share: THIS account's key pair at the ceiling's tier. For
    /// the DEFAULT account `load_credentials_for_account` builds the unlabelled names, so a
    /// single-account store's keys are the strings they always were.
    fn credentials(inputs: &MountInputs<'_>) -> Option<Credentials> {
        load_credentials_for_account(VENUE, Self::tier(inputs), inputs.account, inputs.secrets)
    }
}

impl VenueMount for BinanceVenueMount {
    fn venue(&self) -> &'static str {
        VENUE
    }

    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration {
            addresses_accounts: true,
            process_exclusive: None,
            // The resync supervisor (spot or perp) pokes the reconcile driver after every
            // reconnect's event replay settles.
            takes_recon_trigger: true,
            // `crate::exec`'s `fetch_binance_properties`: one symbol-scoped blocking
            // `exchangeInfo` pre-fetch (`?symbol=`), spot or fapi by the `.P` suffix.
            grid_source: DeclaredGridSource::PerSymbolFetch,
            book_identity: BookIdentity::Undeterminable {
                why: "the store holds an HMAC api key/secret pair and no account identifier; which \
                      account a key belongs to is answerable only by an authenticated call",
            },
            clock: ClockDecl::Wired {
                endpoint: "GET /api/v3/time (public)",
                auth: ClockAuth::Public,
                risk: ClockRisk::SignedTimestamp,
            },
        }
    }

    /// A key pair at the ceiling's tier arms that tier; `held_below_live` is always `None`, because
    /// DEMO is reached only under a ceiling below `live`, where nothing is held below it. With no
    /// pair at that tier the venue is paper — and under a `live` ceiling the cause is the missing
    /// LIVE key set, because this arm never falls back to its demo pair (D1).
    fn resolve(&self, inputs: &MountInputs<'_>) -> Resolution {
        match Self::credentials(inputs) {
            Some(_) => {
                Resolution::Armed { tier: bound_tier(inputs.live_permitted), held_below_live: None }
            }
            None if inputs.live_permitted => Resolution::Paper(PaperCause::LiveCredentialsAbsent),
            None => Resolution::Paper(PaperCause::NoCredentials),
        }
    }

    fn mount(&self, req: MountRequest<'_>) -> MountOutcome {
        mount_with(
            req,
            |c| {
                crate::key_permissions::key_permission_probe(c, crate::spot::MAINNET_REST)
                    .fetch_key_permissions()
            },
            |env, c, symbol| {
                crate::exec::fetch_binance_properties(env, c, symbol).map(|(f, _base)| f)
            },
            |s| {
                Box::new(
                    crate::exec::BinanceExecutionClient::spawn_with_recorder(
                        s.env,
                        s.creds,
                        s.symbol,
                        s.fallback,
                        s.events,
                        s.properties_rec,
                        s.on_reconcile,
                        s.link_id,
                        s.leverage,
                        s.trade_lite_fill,
                    )
                    .with_halt_path(s.halt_path),
                )
            },
        )
    }

    /// `/api/v3/time` on the SAME demo/mainnet host the mount binds — the ceiling's tier (D1), so
    /// the reading is against the clock that judges this mount's signed requests.
    fn server_time_ms(&self, inputs: &MountInputs<'_>, timeout: Duration) -> Result<i64, String> {
        let base =
            if inputs.live_permitted { crate::spot::MAINNET_REST } else { crate::spot::DEMO_REST };
        parse_server_time(&bounded_public_get(VENUE, base, crate::spot::PATH_TIME, timeout)?)
    }

    /// Was `vike-mount`'s legacy `authed_read_probes` over its `AUTHED_READ_MARKETS` row for this
    /// venue: the DEFAULT account's pair at the ceiling's tier (`vike-mount` asks only for a venue
    /// this box would arm, and only on the default account's behalf) and a reconcile client built
    /// PURELY — a signer plus a transport. `vike-mount` reads the balance through it, then hands it
    /// to the identity record at `bound_tier` (inert for binance today — see `bound_tier`).
    fn credential_probe(&self, inputs: &MountInputs<'_>) -> Option<CredentialProbe> {
        let creds = load_credentials_from(VENUE, Self::tier(inputs), inputs.secrets)?;
        let client =
            crate::recon_client::recon_client(&creds, PROBE_SYMBOL, inputs.live_permitted)?;
        Some(CredentialProbe::RecordsIdentity {
            client,
            bound_tier: bound_tier(inputs.live_permitted),
        })
    }
}

#[path = "mount_tests.rs"]
#[cfg(test)]
mod mount_tests;
