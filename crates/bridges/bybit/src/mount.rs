//! bybit's VENUE MOUNT — the `vike_bridge_core::venue_mount::VenueMount` contract over the V5
//! linear-perp exec client and its reconcile client
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md).
//!
//! Moved, behaviour unchanged, from `vike-mount`: the `("bybit", Some(c))` arm of its legacy match
//! (now [`BybitVenueMount::mount`]), its row of the generic-credential arming probe
//! ([`BybitVenueMount::resolve`]), its clock, book-identity and grid-source rows
//! ([`BybitVenueMount::declaration`]), its clock read and parse
//! ([`BybitVenueMount::server_time_ms`]), its `AUTHED_READ_MARKETS` startup probe
//! ([`BybitVenueMount::credential_probe`]), its `build_recon_client` dispatch and its fallback
//! grid.
//!
//! # The ceiling decides the network (D1)
//!
//! `live` means mainnet (docs/decisions/0095-venues-read-no-environment-and-live-means-mainnet.md):
//! a `live` ceiling reads the LIVE key set and binds `api.bybit.com`, every lower ceiling reads DEMO
//! and binds `api-demo.bybit.com` — two SEPARATE hosts, for REST, the private WS and the clock
//! alike. [`MountInputs::live_permitted`] is the whole tier answer, and a `live` ceiling with no
//! LIVE key set stays PAPER: a mainnet host is never signed with demo keys.

use std::sync::{Arc, mpsc};
use std::time::Duration;

use vike_bridge_core::credentials::{
    Credentials, Environment, attribution_code_from, load_credentials_for_account,
    load_credentials_from,
};
use vike_bridge_core::venue_mount::{
    BookIdentity, ClockAuth, ClockDecl, ClockRisk, CredentialProbe, DeclaredGridSource,
    ExecOutcome, LiveExec, MountInputs, MountOutcome, MountRequest, PaperCause, Resolution, Tier,
    VenueDeclaration, VenueMount, bounded_public_get, missing_time_field,
};
use vike_exec::recon::ReconClient;
use vike_exec::{EventSender, ExecutionClient};
use vike_model::SymbolProperties;

use crate::perp::VENUE;

/// The symbol the startup credential probe scopes its reconcile client to — was `vike-mount`'s
/// `AUTHED_READ_MARKETS` row for this venue, mirroring `vike_tradehub::wired_markets::WIRED_MARKETS`. It scopes only
/// that client's order and position reads; the balance read the probe issues is account-wide.
const PROBE_SYMBOL: &str = "BTCUSDT";

/// What a `live` ceiling with no LIVE key set is told — the line `vike-mount`'s legacy prefix
/// logged at its `MainnetNoCreds` site, minus the `{venue}: ` it interpolated (a `const` cannot).
/// The same text as `crates/bridges/binance/src/mount.rs`'s `NO_LIVE_CREDENTIALS`: that prefix
/// logged one line for the venues whose network is their ceiling.
const NO_LIVE_CREDENTIALS: &str = "the ceiling is `live` (MAINNET) but no LIVE credentials are \
     present → staying PAPER (a mainnet host is never signed with demo keys; absent credentials \
     are the live gate)";

/// FALLBACK Bybit linear-perp grid (BTCUSDT-tuned), used ONLY if the Bybit adapter's startup
/// `instruments-info` fetch fails — the adapter fetches the real per-symbol properties. Moved from
/// `vike-mount`'s `fallback.rs`.
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
/// `vike-mount`'s `parse_bybit_time`.
fn parse_server_time(body: &serde_json::Value) -> Result<i64, String> {
    body.get("time").and_then(serde_json::Value::as_i64).ok_or_else(|| missing_time_field("time"))
}

/// The contract's tier for a resolved mainnet verdict.
fn bound_tier(mainnet: bool) -> Tier {
    if mainnet { Tier::Live } else { Tier::Demo }
}

/// `crate::exec::BybitExecutionClient::spawn_with_recorder`'s arguments, in its order — the one
/// value `mount_with` hands its spawn step.
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
    /// The HALT sentinel the client watches — `MountInputs::process.halt_path`, handed to the
    /// client by its `with_halt_path` (decision 0099). Not an argument of `spawn_with_recorder`:
    /// it rides the builder, applied by `mount`'s spawn step.
    halt_path: std::path::PathBuf,
}

/// [`BybitVenueMount::mount`]'s body — the legacy arm, in its order — with the three steps that
/// leave this function taken as parameters: the grid pre-fetch and the exec spawn reach the
/// network (the spawned exec thread dials the venue at once), and the reconcile factory builds a
/// signer and a transport. `mount` passes the real three; a test passes doubles, which is what lets
/// `mount_tests.rs` read the network each step is handed off THIS body rather than off a helper
/// beside it.
fn mount_with(
    req: MountRequest<'_>,
    fetch_grid: impl FnOnce(&Credentials, &str, bool) -> Option<SymbolProperties>,
    build_recon: impl FnOnce(&Credentials, &str, bool) -> Option<Box<dyn ReconClient>>,
    spawn: impl FnOnce(ExecSpawn) -> Box<dyn ExecutionClient + Send>,
) -> MountOutcome {
    // The ONE credential resolution for the whole mount (decision 0095): THIS account's key pair at
    // the tier the ceiling names. It is threaded below into the grid pre-fetch, the reconcile
    // client and the exec spawn, so no spawned thread re-reads anything for itself.
    let Some(c) = BybitVenueMount::credentials(&req.inputs) else {
        // SAFETY (absent credentials are the live gate): a `live` ceiling with no LIVE key set
        // stays PAPER and says so — it never falls back to the DEMO pair.
        if req.inputs.live_permitted {
            tracing::warn!(venue = VENUE, "{VENUE}: {NO_LIVE_CREDENTIALS}");
        }
        return MountOutcome::paper();
    };
    // The ceiling IS the network for this venue (decision 0095): mainnet exactly when it permits
    // the live tier — the same answer `resolve` reports.
    let mainnet = req.inputs.live_permitted;
    // The announcement convention — `venue`, `account`, `tier`, and `⚠ REAL-MONEY: ` on a live
    // tier only — is held for every bridge by `crates/vike-ops/tests/venues/live_mount_line_gate.rs`.
    if mainnet {
        tracing::warn!(
            venue = VENUE,
            account = %req.inputs.account,
            tier = Tier::Live.as_str(),
            "⚠ REAL-MONEY: bybit mounting on MAINNET with LIVE credentials (real funds)"
        );
    } else {
        tracing::warn!(
            venue = VENUE,
            account = %req.inputs.account,
            tier = Tier::Demo.as_str(),
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
    // at the mount, thread the value, never re-read from a spawned thread" idiom `mainnet` and the
    // attribution code follow. This arm used to POST a hardcoded 2x to `/v5/position/set-leverage`
    // while the RiskGate sized every order against `[risk] max_leverage`
    // (`im = 1.0 / max_leverage`), so the account and the gate ran on two different numbers. An
    // UNSET `max_leverage` still yields the historical 2.0 (see `crate::exec::leverage_for` /
    // `vike_bridge_core::leverage`). This is the operator's REQUEST: the exec thread clamps it to
    // `leverageFilter.maxLeverage` for the mounted symbol, read off the `instruments-info` response
    // it already fetches (`clamp_to_venue_cap`) — a cap it cannot read clamps nothing.
    let leverage = crate::exec::leverage_for(req.risk_profile);
    // The `execution.fast` hint — `venue.bybit.fast_exec` (decision 0095), read once from the
    // venue's settings.
    let fast_exec = crate::user_data::fast_exec(req.inputs.settings);
    MountOutcome {
        exec: ExecOutcome::Live(LiveExec {
            client: spawn(ExecSpawn {
                creds: c,
                symbol: req.symbol.to_string(),
                fallback: fallback_properties(),
                events: req.events.clone(),
                // PIT filter recording (opt-in, off by default): the caller-built
                // `PropertiesRecorder::open` handle — `None` unless the root's
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
            contract_size: None,
            margin_mode: None,
            leg_grids: Vec::new(),
        }),
        recon,
        identity: None,
    }
}

/// bybit's mount. `vike_tradehub::registry::REGISTRY` holds `&BybitVenueMount`.
pub struct BybitVenueMount;

impl BybitVenueMount {
    /// The key tier the ceiling selects (D1): LIVE under a `live` ceiling, DEMO otherwise.
    fn tier(inputs: &MountInputs<'_>) -> Environment {
        if inputs.live_permitted { Environment::Live } else { Environment::Demo }
    }

    /// The one gate `resolve` and `mount` share: THIS account's key pair at that tier.
    /// `load_credentials_for_account` reads the same names as `load_credentials_from` for the
    /// default account, and never falls back to the unlabelled pair for a labelled one.
    fn credentials(inputs: &MountInputs<'_>) -> Option<Credentials> {
        load_credentials_for_account(VENUE, Self::tier(inputs), inputs.account, inputs.secrets)
    }
}

impl VenueMount for BybitVenueMount {
    fn venue(&self) -> &'static str {
        VENUE
    }

    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration {
            // `load_credentials_for_account` composes THIS account's key names.
            addresses_accounts: true,
            process_exclusive: None,
            // The resync supervisor pokes the reconcile driver after every reconnect.
            takes_recon_trigger: true,
            // `crates/bridges/bybit/src/exec.rs`'s `fetch_bybit_properties`: one symbol-scoped
            // blocking `instruments-info` pre-fetch per symbol.
            grid_source: DeclaredGridSource::PerSymbolFetch,
            // ⚠ The one CEX venue whose reconcile client does NOT implement
            // `fetch_account_identity`, deliberately: its demo key answered `retCode 33004` (*Your
            // api key has expired*) when the probe went to measure what its response names, so
            // nothing was measured and nothing was written. Guessing the field names from the
            // venue docs is the `canWithdraw` trap (`vike_bridge_core::key_permissions`), and the
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

    /// The legacy arming probe's row for this venue, answered from the SAME credential read `mount`
    /// acts on: a key pair at the ceiling's tier arms that tier; with none, a `live` ceiling is
    /// `LiveCredentialsAbsent` — this arm never falls back to its DEMO pair — and any lower one
    /// `NoCredentials`. `held_below_live` is always `None`: DEMO is reached only under a ceiling
    /// below `live`, where nothing is being held below it.
    fn resolve(&self, inputs: &MountInputs<'_>) -> Resolution {
        match (Self::credentials(inputs), inputs.live_permitted) {
            (Some(_), live) => Resolution::Armed { tier: bound_tier(live), held_below_live: None },
            (None, true) => Resolution::Paper(PaperCause::LiveCredentialsAbsent),
            (None, false) => Resolution::Paper(PaperCause::NoCredentials),
        }
    }

    fn mount(&self, req: MountRequest<'_>) -> MountOutcome {
        mount_with(
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

    /// `GET /v5/market/time` on the host the mount binds: [`crate::perp::endpoints`], the one host
    /// selection the exec client, the reconcile client and the grid pre-fetch all take, under the
    /// same ceiling. Demo and mainnet are two SEPARATE hosts here, so the tier matters. Was
    /// `vike-mount`'s `bybit_time`.
    fn server_time_ms(&self, inputs: &MountInputs<'_>, timeout: Duration) -> Result<i64, String> {
        let (rest_base, _) = crate::perp::endpoints(inputs.live_permitted);
        parse_server_time(&bounded_public_get(VENUE, rest_base, crate::perp::PATH_TIME, timeout)?)
    }

    /// Was `vike-mount`'s `AUTHED_READ_MARKETS` row for this venue: the DEFAULT account's pair at
    /// the ceiling's tier — the account `vike-mount` records the answer against — and a reconcile
    /// client built PURELY (a signer and a transport, no network). `vike-mount` reads the balance,
    /// then records which account the key is at `bound_tier`, which for bybit asks nothing (see the
    /// declaration's book-identity note).
    fn credential_probe(&self, inputs: &MountInputs<'_>) -> Option<CredentialProbe> {
        credential_probe_with(inputs, crate::recon_client::recon_client)
    }
}

/// [`BybitVenueMount::credential_probe`]'s body, with the reconcile factory taken as a parameter:
/// the step that decides which HOST the probe's signed balance read is sent to. The probe passes
/// the real factory; a test passes a double that records the key and the network it is handed,
/// which is how `mount_tests.rs` pins that the probe signs with the pair of the tier it reports, on
/// that tier's host.
fn credential_probe_with(
    inputs: &MountInputs<'_>,
    build_recon: impl FnOnce(&Credentials, &str, bool) -> Option<Box<dyn ReconClient>>,
) -> Option<CredentialProbe> {
    // The ceiling's verdict, bound once for the host the balance read is signed against and the
    // tier reported beside it — the legacy probe's `probe_mainnet`, which for this venue was the
    // ceiling alone.
    let mainnet = inputs.live_permitted;
    let creds = load_credentials_from(VENUE, BybitVenueMount::tier(inputs), inputs.secrets)?;
    let client = build_recon(&creds, PROBE_SYMBOL, mainnet)?;
    Some(CredentialProbe::RecordsIdentity { client, bound_tier: bound_tier(mainnet) })
}

#[path = "mount_tests.rs"]
#[cfg(test)]
mod mount_tests;
