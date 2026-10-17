//! bybit's VENUE ADAPTER — the `vike_bridge_core::venue_adapter::VenueAdapter` contract over the V5
//! linear-perp exec client and its reconcile client
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md).
//!
//! Moved, behaviour unchanged, from `vike-tradehub-core`: the `("bybit", Some(c))` arm of its legacy match
//! (now [`BybitVenueAdapter::start`]), its row of the generic-credential arming probe
//! ([`BybitVenueAdapter::resolve`]), its clock, book-identity and grid-source rows
//! ([`BybitVenueAdapter::declaration`]), its clock read and parse
//! ([`BybitVenueAdapter::server_time_ms`]), its `AUTHED_READ_MARKETS` startup probe
//! ([`BybitVenueAdapter::credential_probe`]), its `build_recon_client` dispatch and its fallback
//! grid.
//!
//! # The account's tier decides the network (D1)
//!
//! `live` means mainnet (docs/decisions/0095-venues-read-no-environment-and-live-means-mainnet.md):
//! a `live` account binds `api.bybit.com`, every lower tier binds `api-demo.bybit.com` — two
//! SEPARATE hosts, for REST, the private WS and the clock alike. [`AdapterInputs::tier`] is the whole
//! tier answer. The key pair is the started ACCOUNT's (`AdapterInputs::credentials`, its `API_KEY` +
//! `API_SECRET` fields — decision 0156 item 6): the account has ONE tier, so its keys are that
//! tier's, and a `live` account with no key pair does NOT RUN with nothing to fall back to.

use std::sync::{Arc, mpsc};
use std::time::Duration;

use vike_bridge_core::venue_adapter::{
    AdapterInputs, AdapterOutcome, AdapterRequest, BookIdentity, ClockAuth, ClockDecl, ClockRisk,
    CredentialProbe, DeclaredGridSource, ExecOutcome, LiveExec, PaperCause, Resolution,
    VenueAdapter, VenueDeclaration, bounded_public_get, missing_time_field,
};
use vike_exec::recon::ReconClient;
use vike_exec::{EventSender, ExecutionClient};
use vike_model::{AccountTier, SymbolProperties};
use vike_secrets::credentials::{Credentials, api_credentials, attribution_code_from};

use crate::perp::VENUE;

/// The symbol the startup credential probe scopes its reconcile client to — was `vike-tradehub-core`'s
/// `AUTHED_READ_MARKETS` row for this venue.
/// It is this venue's row of `crates/vike-venues/src/venue_wiring.rs`'s `VENUE_WIRING` — the
/// symbol the daemon's default engine starts on — read at compile time, so the probe cannot drift
/// from the engine market and a row that loses its engine fails this crate's build. It scopes only
/// that client's order and position reads; the balance read the probe issues is account-wide.
const PROBE_SYMBOL: &str = match vike_venues::venue_wiring::wired_symbol(VENUE) {
    Some(symbol) => symbol,
    None => {
        panic!("this venue's credential probe needs an engine row in vike_model's VENUE_WIRING")
    }
};

/// What a `live` account with no key pair is told — the line `vike-tradehub-core`'s legacy prefix
/// logged at its `MainnetNoCreds` site, minus the `{venue}: ` it interpolated (a `const` cannot).
/// The same text as `crates/bridges/binance/src/adapter.rs`'s `NO_LIVE_CREDENTIALS`: that prefix
/// logged one line for the venues whose network is their account's tier.
const NO_LIVE_CREDENTIALS: &str = "the account's tier is `live` (MAINNET) but no LIVE \
     credentials are present → staying PAPER (a mainnet host is never signed with demo keys; \
     absent credentials are the live gate)";

/// FALLBACK Bybit linear-perp grid (BTCUSDT-tuned), used ONLY if the Bybit adapter's startup
/// `instruments-info` fetch fails — the adapter fetches the real per-symbol properties. Moved from
/// `vike-tradehub-core`'s `fallback.rs`.
fn fallback_properties() -> SymbolProperties {
    SymbolProperties {
        tick_size: 0.1,
        step_size: 0.001,
        min_qty: 0.001,
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

/// bybit's v5 envelope. ⚠ The stamp is the TOP-LEVEL `"time"` NUMBER; `result.timeNano` is a
/// NANOSECOND string and `result.timeSecond` a SECONDS string, so reading the wrong one is off by
/// a factor of a million or a thousand while still looking like a plausible integer. Was
/// `vike-tradehub-core`'s `parse_bybit_time`.
fn parse_server_time(body: &serde_json::Value) -> Result<i64, String> {
    body.get("time").and_then(serde_json::Value::as_i64).ok_or_else(|| missing_time_field("time"))
}

/// The contract's tier for a resolved mainnet verdict.
fn bound_tier(mainnet: bool) -> AccountTier {
    if mainnet { AccountTier::Live } else { AccountTier::Demo }
}

/// **The network the account's tier selects** (D1): mainnet for [`AccountTier::Live`], the demo
/// network for [`AccountTier::Demo`]. [`AccountTier::Paper`] never reaches a bridge (`vike-tradehub-core`
/// returns before consulting one) and reads as the not-live branch, as `live_permitted == false`
/// did before decision 0129.
fn is_mainnet(tier: AccountTier) -> bool {
    match tier {
        AccountTier::Live => true,
        AccountTier::Demo | AccountTier::Paper => false,
    }
}

/// `crate::exec::BybitExecutionClient::spawn_with_recorder`'s arguments, in its order — the one
/// value `start_with` hands its spawn step.
struct ExecSpawn {
    creds: Credentials,
    symbol: String,
    fallback: SymbolProperties,
    events: EventSender,
    properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
    recon_trigger: Option<mpsc::Sender<()>>,
    broker_id: Option<String>,
    mainnet: bool,
    leverage: f64,
    fast_exec: bool,
    /// The HALT sentinel the client watches — `AdapterInputs::process.halt_path`, handed to the
    /// client by its `with_halt_path` (decision 0099). Not an argument of `spawn_with_recorder`:
    /// it rides the builder, applied by `start`'s spawn step.
    halt_path: std::path::PathBuf,
}

/// [`BybitVenueAdapter::start`]'s body — the legacy arm, in its order — with the three steps that
/// leave this function taken as parameters: the grid pre-fetch and the exec spawn reach the
/// network (the spawned exec thread dials the venue at once), and the reconcile factory builds a
/// signer and a transport. `start` passes the real three; a test passes doubles, which is what lets
/// `adapter_tests.rs` read the network each step is handed off THIS body rather than off a helper
/// beside it.
fn start_with(
    req: AdapterRequest<'_>,
    fetch_grid: impl FnOnce(&Credentials, &str, bool) -> Option<SymbolProperties>,
    build_recon: impl FnOnce(&Credentials, &str, bool) -> Option<Box<dyn ReconClient>>,
    spawn: impl FnOnce(ExecSpawn) -> Box<dyn ExecutionClient + Send>,
) -> AdapterOutcome {
    // The ONE credential resolution for the whole start (decision 0095): THIS account's key pair,
    // which is the tier the account row names. It is threaded below into the grid pre-fetch, the
    // reconcile client and the exec spawn, so no spawned thread re-reads anything for itself.
    let Some(c) = BybitVenueAdapter::credentials(&req.inputs) else {
        // SAFETY (absent credentials are the live gate): a `live` account with no key pair stays
        // PAPER and says so.
        if is_mainnet(req.inputs.tier) {
            tracing::warn!(venue = VENUE, "{VENUE}: {NO_LIVE_CREDENTIALS}");
        }
        return AdapterOutcome::paper();
    };
    // The tier IS the network for this venue (decision 0095): mainnet exactly when it permits
    // the live tier — the same answer `resolve` reports.
    let mainnet = is_mainnet(req.inputs.tier);
    // The announcement convention — `venue`, `account`, `tier`, and `⚠ REAL-MONEY: ` on a live
    // tier only — is held for every bridge by `crates/vike-ops/tests/venues/live_mount_line_gate.rs`.
    if mainnet {
        tracing::warn!(
            venue = VENUE,
            account = %req.inputs.account,
            tier = AccountTier::Live.as_str(),
            "⚠ REAL-MONEY: bybit mounting on MAINNET with LIVE credentials (real funds)"
        );
    } else {
        tracing::warn!(
            venue = VENUE,
            account = %req.inputs.account,
            tier = AccountTier::Demo.as_str(),
            "bybit: DEMO credentials present → LIVE exec client (real demo orders)"
        );
    }
    // Live RiskGate from the venue's REAL grid (PR-2b): one blocking pre-fetch (a duplicate of the
    // adapter's own in-thread fetch — acceptable, startup-only). On failure the permissive default
    // stands (byte-identical to pre-PR-2b behavior).
    let grid = fetch_grid(&c, req.symbol, mainnet);
    // Reconcile handle (audit A1 item 4): a FRESH stateless-HMAC REST client dedicated to reconcile
    // reads, built from the SAME `c` before it moves into the spawn below (the factory only borrows
    // it). Pure to construct, so NOT gated on `recon_enabled`.
    let recon = build_recon(&c, req.symbol, mainnet);
    // Fee-attribution FD-broker code (unified cross-venue attribution, task 5), resolved ONCE from
    // the credential map: `BYBIT_BROKER_CODE`/`BYBIT_BUILDER_CODE`, validated against bybit's
    // `AttributionMechanic::Header { name: "X-Referer" }`. Absent/invalid degrades to `None` — no
    // `X-Referer` header at all.
    let broker_id = attribution_code_from(req.inputs.secrets, VENUE);
    // ACCOUNT LEVERAGE, resolved ONCE here from the operator's `[risk]` budget — the same "resolve
    // at the start, thread the value, never re-read from a spawned thread" idiom `mainnet` and the
    // attribution code follow. This arm used to POST a hardcoded 2x to `/v5/position/set-leverage`
    // while the RiskGate sized every order against `[risk] max_leverage`
    // (`im = 1.0 / max_leverage`), so the account and the gate ran on two different numbers. An
    // UNSET `max_leverage` still yields the historical 2.0 (see `crate::exec::leverage_for` /
    // `vike_bridge_core::leverage`). This is the operator's REQUEST: the exec thread clamps it to
    // `leverageFilter.maxLeverage` for the engine symbol, read off the `instruments-info` response
    // it already fetches (`clamp_to_venue_cap`) — a cap it cannot read clamps nothing.
    let leverage = crate::exec::leverage_for(req.risk_profile);
    // The `execution.fast` hint — `venue.bybit.fast_exec` (decision 0095), read once from the
    // venue's settings.
    let fast_exec = crate::user_data::fast_exec(req.inputs.settings);
    AdapterOutcome {
        exec: ExecOutcome::Live(LiveExec {
            client: spawn(ExecSpawn {
                creds: c,
                symbol: req.symbol.to_string(),
                fallback: fallback_properties(),
                events: req.events.clone(),
                // PIT filter recording (opt-in, off by default): the caller-built
                // `vike_store::open_properties_recorder` handle — `None` unless the root's
                // `flags.record_properties` row is on (and the binary carries the store backend),
                // so the disabled path is byte-identical.
                properties_rec: req.properties_rec,
                recon_trigger: req.recon_trigger,
                broker_id,
                mainnet,
                leverage,
                fast_exec,
                halt_path: req.inputs.process.halt_path.clone(),
            }),
            bound_tier: bound_tier(mainnet),
            grid,
            contract_value: None,
            margin_mode: None,
            leg_grids: Vec::new(),
        }),
        recon,
        identity: None,
    }
}

/// bybit's start. `vike_tradehub::registry::REGISTRY` holds `&BybitVenueAdapter`.
pub struct BybitVenueAdapter;

impl BybitVenueAdapter {
    /// The one gate `resolve`, `start` and the startup probe share: THIS account's key pair — its
    /// `API_KEY` and `API_SECRET` fields (`vike_secrets::credentials::api_credentials`), which are
    /// its tier's because the account has one tier.
    fn credentials(inputs: &AdapterInputs<'_>) -> Option<Credentials> {
        api_credentials(&inputs.credentials, VENUE)
    }
}

impl VenueAdapter for BybitVenueAdapter {
    fn venue(&self) -> &'static str {
        VENUE
    }

    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration {
            // The arm reads THIS account's key pair (`AdapterInputs::credentials`).
            addresses_accounts: true,
            process_exclusive: None,
            // The resync supervisor fires the recon trigger after every reconnect.
            takes_recon_trigger: true,
            // `crates/bridges/bybit/src/exec.rs`'s `fetch_bybit_properties`: one symbol-scoped
            // blocking `instruments-info` pre-fetch per symbol.
            grid_source: DeclaredGridSource::PerSymbolFetch,
            // ⚠ The one CEX venue whose reconcile client does NOT implement
            // `fetch_account_identity`, deliberately: its demo key answered `retCode 33004` (*Your
            // api key has expired*) when the probe went to measure what its response names, so
            // nothing was measured and nothing was written. Guessing the field names from the
            // venue docs is the `canWithdraw` trap (`vike_secrets::key_permissions`), and the
            // whole point of that workstream was not to take it.
            book_identity: BookIdentity::Undeterminable {
                why: "the store holds an HMAC api key/secret pair and no account identifier; a bybit \
                      sub-account is selected BY THE KEY, which names it nowhere offline",
            },
            clock: ClockDecl::Wired {
                endpoint: "GET /v5/market/time (public)",
                auth: ClockAuth::Public,
                risk: ClockRisk::SignedTimestamp,
            },
        }
    }

    /// The legacy arming probe's row for this venue, answered from the SAME credential read `start`
    /// acts on: the account's key pair arms the account's tier; with none, a `live` tier is
    /// `LiveCredentialsAbsent` and any lower one `NoCredentials`. `held_below_live` is always
    /// `None`: DEMO is reached only under a tier below `live`, where nothing is being held below
    /// it.
    fn resolve(&self, inputs: &AdapterInputs<'_>) -> Resolution {
        match (Self::credentials(inputs), is_mainnet(inputs.tier)) {
            (Some(_), live) => Resolution::Armed { tier: bound_tier(live), held_below_live: None },
            (None, true) => Resolution::Paper(PaperCause::LiveCredentialsAbsent),
            (None, false) => Resolution::Paper(PaperCause::NoCredentials),
        }
    }

    fn start(&self, req: AdapterRequest<'_>) -> AdapterOutcome {
        start_with(
            req,
            crate::exec::fetch_bybit_properties,
            crate::recon_client::recon_client,
            |s| {
                Box::new(
                    crate::exec::BybitExecutionClient::spawn_with_recorder(
                        s.creds,
                        s.symbol,
                        s.fallback,
                        s.events,
                        s.properties_rec,
                        s.recon_trigger,
                        s.broker_id,
                        s.mainnet,
                        s.leverage,
                        s.fast_exec,
                    )
                    .with_halt_path(s.halt_path),
                )
            },
        )
    }

    /// `GET /v5/market/time` on the host the start binds: [`crate::perp::endpoints`], the one host
    /// selection the exec client, the reconcile client and the grid pre-fetch all take, under the
    /// same tier. Demo and mainnet are two SEPARATE hosts here, so the tier matters. Was
    /// `vike-tradehub-core`'s `bybit_time`.
    fn server_time_ms(&self, inputs: &AdapterInputs<'_>, timeout: Duration) -> Result<i64, String> {
        let (rest_base, _) = crate::perp::endpoints(is_mainnet(inputs.tier));
        parse_server_time(&bounded_public_get(VENUE, rest_base, crate::perp::PATH_TIME, timeout)?)
    }

    /// Was `vike-tradehub-core`'s `AUTHED_READ_MARKETS` row for this venue: the probed account's pair — the
    /// account `vike-tradehub-core` records the answer against — and a reconcile client built PURELY (a
    /// signer and a transport, no network). `vike-tradehub-core` reads the balance,
    /// then records which account the key is at `bound_tier`, which for bybit asks nothing (see the
    /// declaration's book-identity note).
    fn credential_probe(&self, inputs: &AdapterInputs<'_>) -> Option<CredentialProbe> {
        credential_probe_with(inputs, crate::recon_client::recon_client)
    }
}

/// [`BybitVenueAdapter::credential_probe`]'s body, with the reconcile factory taken as a parameter:
/// the step that decides which HOST the probe's signed balance read is sent to. The probe passes
/// the real factory; a test passes a double that records the key and the network it is handed,
/// which is how `adapter_tests.rs` pins that the probe signs with the account's pair, on the host of
/// the tier it reports.
fn credential_probe_with(
    inputs: &AdapterInputs<'_>,
    build_recon: impl FnOnce(&Credentials, &str, bool) -> Option<Box<dyn ReconClient>>,
) -> Option<CredentialProbe> {
    // The tier's verdict, bound once for the host the balance read is signed against and the
    // tier reported beside it — the legacy probe's `probe_mainnet`, which for this venue was the
    // tier alone.
    let mainnet = is_mainnet(inputs.tier);
    let creds = BybitVenueAdapter::credentials(inputs)?;
    let client = build_recon(&creds, PROBE_SYMBOL, mainnet)?;
    Some(CredentialProbe::RecordsIdentity { client, bound_tier: bound_tier(mainnet) })
}

#[path = "adapter_tests.rs"]
#[cfg(test)]
mod adapter_tests;
