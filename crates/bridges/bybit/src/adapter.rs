//! bybit's VENUE ADAPTER — the `vike_bridge_core::venue_adapter::VenueAdapter` contract over the V5
//! linear-perp exec client and its reconcile client
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md).
//!
//! Moved, behaviour unchanged, from `vike-tradehub-core`: the `("bybit", Some(c))` arm of its legacy match
//! (now [`VenueAdapterBybit::start`]), its row of the generic-credential arming probe
//! ([`VenueAdapterBybit::resolve`]), its clock, book-identity and grid-source rows
//! ([`VenueAdapterBybit::declaration`]), its clock read and parse
//! ([`VenueAdapterBybit::server_time_ms`]), its `AUTHED_READ_MARKETS` startup probe
//! ([`VenueAdapterBybit::credential_probe`]) and its `build_recon_client` dispatch. Its fallback grid is
//! gone (owner, 2026-11-13): a symbol whose row has not loaded is refused in words.
//!
//! # The account's tier decides the network (D1)
//!
//! `live` means mainnet (docs/decisions/0095-venues-read-no-environment-and-live-means-mainnet.md):
//! a `live` account binds its `live` `venue_url` rows (`api.bybit.com` in the seed), every lower tier
//! its `demo` rows (`api-demo.bybit.com`) — two SEPARATE hosts, for REST, the private WS and the clock
//! alike, looked up ONCE per start (`crate::execution::rest::endpoints`); a missing row is NOT RUNNING with the
//! row named. [`AdapterInputs::tier`] is the whole
//! tier answer. The key pair is the started ACCOUNT's (`AdapterInputs::credentials`, its `API_KEY` +
//! `API_SECRET` fields — decision 0156 item 6): the account has ONE tier, so its keys are that
//! tier's, and a `live` account with no key pair does NOT RUN with nothing to fall back to.

use std::sync::{Arc, mpsc};
use std::time::Duration;

use vike_bridge_core::leverage_read::LeverageWriteError;
use vike_bridge_core::reconcile_admitted::ReconcileAdmittedSymbols;
use vike_bridge_core::signer::VenueClock;
use vike_bridge_core::venue_adapter::{
    AdapterInputs, AdapterOutcome, AdapterRequest, ArmingResolution, BookIdentityRule,
    ClockAuthentication, ClockDeclaration, CredentialProbe, VenueAdapter, VenueAdapterDeclaration,
    VenueAdapterExecutionOutcome, VenueAdapterExecutionRunning, bound_tier, bounded_public_get,
    missing_time_field, report_address_refused, report_live_credentials_absent,
    resolve_one_key_set,
};
use vike_execution::recon::ReconcileClient;
use vike_execution::{EventSender, ExecutionClient};
use vike_model::{AccountTier, ClockRisk, GridSource, VenueNetwork};
use vike_secrets::credentials::{Credentials, api_credentials, attribution_code_from};

use crate::execution::rest::{BybitUrls, VENUE};

/// bybit's v5 envelope's stamp ([`crate::transport::client::server_time`], the one reading of it, which the signing clock
/// also takes). Was `vike-tradehub-core`'s `parse_bybit_time`.
fn parse_server_time(body: &serde_json::Value) -> Result<i64, String> {
    crate::transport::client::server_time(body).ok_or_else(|| missing_time_field("time"))
}

/// `crate::execution::spawn_with_recorder`'s arguments, in its order — the one
/// value `start_with` hands its spawn step.
struct ExecutionSpawn {
    creds: Credentials,
    symbol: String,
    events: EventSender,
    properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
    reconcile_trigger: Option<mpsc::Sender<()>>,
    broker_id: Option<String>,
    endpoints: BybitUrls,
    leverage: f64,
    fast_execution: bool,
    /// The engine's admitted-symbol list, the SAME `Arc` the reconcile client is told each admission
    /// through: the exec thread's reconnect-resync replays the history of each symbol on it.
    admitted: Arc<ReconcileAdmittedSymbols>,
    /// The HALT sentinel the client watches — `AdapterInputs::process.halt_path`, handed to the
    /// client by its `with_halt_path` (decision 0099). Not an argument of `spawn_with_recorder`:
    /// it rides the builder, applied by `start`'s spawn step.
    halt_path: std::path::PathBuf,
    /// The start's ONE signing clock (owner ruling B-4): the grid pre-fetch and the reconcile client sign on it
    /// too, so the first of them to sign reads the venue's time for all.
    clock: VenueClock,
}

/// [`VenueAdapterBybit::start`]'s body — the legacy arm, in its order — with the three steps that
/// leave this function taken as parameters: the grid pre-fetch and the exec spawn reach the
/// network (the spawned exec thread dials the venue at once), and the reconcile factory builds a
/// signer and a transport. `start` passes the real three; a test passes doubles, which is what lets
/// `adapter_tests.rs` read the network each step is handed off THIS body rather than off a helper
/// beside it.
fn start_with(
    req: AdapterRequest<'_>,
    fetch_grid: impl FnOnce(
        &Credentials,
        &str,
        &str,
        &VenueClock,
    ) -> Option<crate::execution::rest::BybitInstrument>,
    build_reconcile: impl FnOnce(
        &Credentials,
        &str,
        &str,
        Arc<ReconcileAdmittedSymbols>,
        &VenueClock,
    ) -> Option<Box<dyn ReconcileClient>>,
    spawn: impl FnOnce(ExecutionSpawn) -> Box<dyn ExecutionClient + Send>,
) -> AdapterOutcome {
    // The ONE credential resolution for the whole start (decision 0095): THIS account's key pair,
    // which is the tier the account row names. It is threaded below into the grid pre-fetch, the
    // reconcile client and the exec spawn, so no spawned thread re-reads anything for itself.
    let Some(c) = VenueAdapterBybit::credentials(&req.inputs) else {
        // SAFETY (absent credentials are the live gate): a `live` account with no key pair stays
        // PAPER and says so.
        report_live_credentials_absent(VENUE, req.inputs.account, req.inputs.tier);
        return AdapterOutcome::paper();
    };
    // The tier IS the network for this venue (decision 0095): mainnet exactly when it permits
    // the live tier — the same answer `resolve` reports — and the hosts are the account's rows at
    // that type, looked up ONCE here. A missing row: NOT RUNNING, the row named.
    let endpoints =
        match crate::execution::rest::endpoints(req.inputs.settings.urls(), req.inputs.tier) {
            Ok(endpoints) => endpoints,
            Err(refusal) => {
                report_address_refused(VENUE, req.inputs.account, refusal);
                return AdapterOutcome::paper();
            }
        };
    let bound = bound_tier(VenueNetwork::for_tier(req.inputs.tier));
    // The announcement convention — `venue`, `account`, `tier`, and `⚠ REAL-MONEY: ` on a live
    // tier only — is held for every bridge by `crates/vike-ops/tests/venues/live_mount_line_gate.rs`.
    if bound == AccountTier::Live {
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
    //
    // The same row says what a unit of the quantity is WORTH (decisions 0170, 0258): a coin-margined
    // mount reports the inverse contract value (one contract is one US dollar), from which the engine
    // keeps its books in the settle coin. ⚠ NO ROW, NO MOUNT for a name that reads as coin-margined: its
    // kind is never defaulted to linear, so the account does NOT RUN, with the reason (the supervisor
    // retries), as deribit's start does. A linear mount reports nothing, as before.
    // ONE signing clock for this start (owner ruling B-4): every signed request below and the private socket's
    // login stamp the venue's time, read by the first of them to sign (this grid pre-fetch).
    let clock = VenueClock::default();
    // The mounted symbol's BOOK is its KEPT row's (`crate::history::Category::of_kept`, the owner's ruling of
    // 2026-11-13): never read off its name, so with no row the account does NOT RUN, with the reason (the supervisor
    // retries, and a list the datahub refreshes is read within a minute).
    if crate::history::Category::of_kept(req.symbol).is_none() {
        tracing::error!(
            venue = VENUE,
            account = %req.inputs.account,
            symbol = req.symbol,
            "bybit: REFUSED — the kept instrument list (the datahub's <project>/instruments/bybit.json) carries no \
             row of this symbol, so the book its orders are signed in is unknown; the account is NOT RUNNING until \
             the list holds it (its weekly refresh, or the Data Manager's Refresh)"
        );
        return AdapterOutcome::paper();
    }
    let row = fetch_grid(&c, req.symbol, &endpoints.rest, &clock);
    let contract_value = match crate::execution::mount_money(req.symbol, row.as_ref()) {
        Ok(money) => money.map(|(value, _)| value),
        Err(why) => {
            tracing::error!(
                venue = VENUE,
                account = %req.inputs.account,
                symbol = req.symbol,
                "bybit: REFUSED — {why}; the account is NOT RUNNING until the row can be read"
            );
            return AdapterOutcome::paper();
        }
    };
    let grid = row.map(|r| r.properties);
    // Reconcile handle (audit A1 item 4): a FRESH stateless-HMAC REST client dedicated to reconcile
    // reads, built from the SAME `c` before it moves into the spawn below (the factory only borrows
    // it). Pure to construct, so NOT gated on `recon_enabled`.
    //
    // The admitted-symbol list is ONE `Arc`: this client is told each admission (`ReconcileClient::admit_symbol`),
    // and the exec thread's reconnect-resync and gap-sentinel read the same list, so a symbol the engine
    // admits is also one whose missed fills are replayed (decision 0174's reopener).
    let admitted = Arc::<ReconcileAdmittedSymbols>::default();
    let recon = build_reconcile(&c, req.symbol, &endpoints.rest, Arc::clone(&admitted), &clock);
    // Fee-attribution FD-broker code (unified cross-venue attribution, task 5), resolved ONCE from
    // the credential map: `BYBIT_BROKER_CODE`/`BYBIT_BUILDER_CODE`, validated against bybit's
    // `AttributionMechanic::Header { name: "X-Referer" }`. Absent/invalid degrades to `None` — no
    // `X-Referer` header at all.
    let broker_id = attribution_code_from(req.inputs.secrets, VENUE);
    // ACCOUNT LEVERAGE, resolved ONCE here from the operator's `[risk]` budget — the same "resolve
    // at the start, thread the value, never re-read from a spawned thread" idiom `endpoints` and the
    // attribution code follow. This arm used to POST a hardcoded 2x to `/v5/position/set-leverage`
    // while the RiskGate sized every order against `[risk] max_leverage`
    // (`initial_margin = 1.0 / max_leverage`), so the account and the gate ran on two different numbers. An
    // UNSET `max_leverage` still yields the historical 2.0 (see `crate::execution::leverage_for` /
    // `vike_bridge_core::leverage`). This is the operator's REQUEST: the exec thread clamps it to
    // `leverageFilter.maxLeverage` for the engine symbol, read off the `instruments-info` response
    // it already fetches (`clamp_to_venue_cap`) — a cap it cannot read clamps nothing.
    let leverage = crate::execution::leverage_for(req.risk_profile);
    // The `execution.fast` hint — `venue.bybit.fast_exec` (decision 0095), read once from the
    // venue's settings.
    let fast_execution = crate::execution::user_data::fast_execution(req.inputs.settings);
    AdapterOutcome {
        exec: VenueAdapterExecutionOutcome::Running(VenueAdapterExecutionRunning {
            client: spawn(ExecutionSpawn {
                creds: c,
                symbol: req.symbol.to_string(),
                events: req.events.clone(),
                // PIT filter recording (opt-in): the caller-built handle, `None` unless a root
                // builds one over its store. No root does: `flags.record_properties` (on by default)
                // arms only the datahub's own tape, so this path is byte-identical.
                properties_rec: req.properties_rec,
                reconcile_trigger: req.reconcile_trigger,
                broker_id,
                endpoints,
                leverage,
                fast_execution,
                admitted,
                halt_path: req.inputs.process.halt_path.clone(),
                clock,
            }),
            bound_tier: bound,
            grid,
            contract_value,
            margin_mode: None,
            leg_grids: Vec::new(),
        }),
        reconcile: recon,
        identity: None,
    }
}

/// bybit's start. `vike_tradehub::registry::REGISTRY` holds `&VenueAdapterBybit`.
pub struct VenueAdapterBybit;

impl VenueAdapterBybit {
    /// The one gate `resolve`, `start` and the startup probe share: THIS account's key pair — its
    /// `API_KEY` and `API_SECRET` fields (`vike_secrets::credentials::api_credentials`), which are
    /// its tier's because the account has one tier.
    fn credentials(inputs: &AdapterInputs<'_>) -> Option<Credentials> {
        api_credentials(&inputs.credentials, VENUE)
    }
}

impl VenueAdapter for VenueAdapterBybit {
    fn venue(&self) -> &'static str {
        VENUE
    }

    fn declaration(&self) -> VenueAdapterDeclaration {
        VenueAdapterDeclaration {
            // The arm reads THIS account's key pair (`AdapterInputs::credentials`).
            addresses_accounts: true,
            process_exclusive: None,
            // The resync supervisor fires the recon trigger after every reconnect.
            takes_reconcile_trigger: true,
            // `crates/bridges/bybit/src/execution/mod.rs`'s `fetch_bybit_properties`: one symbol-scoped
            // blocking `instruments-info` pre-fetch per symbol.
            grid_source: GridSource::PerSymbolFetch,
            // ⚠ The one CEX venue whose reconcile client does NOT implement
            // `fetch_account_identity`, deliberately: its demo key answered `retCode 33004` (*Your
            // api key has expired*) when the probe went to measure what its response names, so
            // nothing was measured and nothing was written. Guessing the field names from the
            // venue docs is the `canWithdraw` trap (`vike_secrets::key_permissions`), and the
            // whole point of that workstream was not to take it.
            book_identity: BookIdentityRule::Undeterminable {
                why: "the store holds an HMAC api key/secret pair and no account identifier; a bybit \
                      sub-account is selected BY THE KEY, which names it nowhere offline",
            },
            clock: ClockDeclaration::Wired {
                endpoint: "GET /v5/market/time (public)",
                auth: ClockAuthentication::Public,
                risk: ClockRisk::SignedTimestamp,
            },
        }
    }

    /// The legacy arming probe's row for this venue, answered from the SAME credential read `start`
    /// acts on: the account's key pair arms the account's tier; with none, a `live` tier is
    /// `LiveCredentialsAbsent` and any lower one `NoCredentials`. `held_below_live` is always
    /// `None`: DEMO is reached only under a tier below `live`, where nothing is being held below
    /// it.
    fn resolve(&self, inputs: &AdapterInputs<'_>) -> ArmingResolution {
        resolve_one_key_set(
            Self::credentials(inputs).is_some(),
            VenueNetwork::for_tier(inputs.tier),
        )
    }

    fn start(&self, req: AdapterRequest<'_>) -> AdapterOutcome {
        start_with(
            req,
            |creds, symbol, rest, clock| {
                crate::execution::fetch_bybit_instrument(creds, symbol, rest, clock).ok()
            },
            crate::reconcile::reconcile_client_sharing,
            |s| {
                Box::new(
                    crate::execution::spawn_with_recorder(
                        s.creds,
                        s.symbol,
                        s.events,
                        s.properties_rec,
                        s.reconcile_trigger,
                        s.broker_id,
                        s.endpoints,
                        s.leverage,
                        s.fast_execution,
                        s.admitted,
                        s.clock,
                    )
                    .with_halt_path(s.halt_path),
                )
            },
        )
    }

    /// `GET /v5/market/time` on the host the start binds: [`crate::execution::rest::endpoints`], the one host
    /// lookup the exec client, the reconcile client and the grid pre-fetch all take, under the
    /// same tier. Demo and mainnet are two SEPARATE hosts here, so the tier matters. Was
    /// `vike-tradehub-core`'s `bybit_time`.
    fn server_time_ms(&self, inputs: &AdapterInputs<'_>, timeout: Duration) -> Result<i64, String> {
        let rest_base = endpoints_of(inputs)?.rest;
        parse_server_time(&bounded_public_get(
            VENUE,
            &rest_base,
            crate::execution::rest::PATH_TIME,
            timeout,
        )?)
    }

    /// Was `vike-tradehub-core`'s `AUTHED_READ_MARKETS` row for this venue: the probed account's pair — the
    /// account `vike-tradehub-core` records the answer against — and a reconcile client built PURELY (a
    /// signer and a transport, no network). `vike-tradehub-core` reads the balance,
    /// then records which account the key is at `bound_tier`, which for bybit asks nothing (see the
    /// declaration's book-identity note).
    fn credential_probe(&self, inputs: &AdapterInputs<'_>) -> Option<CredentialProbe> {
        credential_probe_with(inputs, crate::reconcile::recon_client)
    }

    /// The account's leverage for a symbol the engine was not mounted on: the exec half's own
    /// set-leverage ([`crate::execution::write_symbol_leverage`]) with THIS account's key pair, on the host
    /// its tier selects, and the attribution code a start resolves. No key pair, no call.
    fn set_symbol_leverage(
        &self,
        inputs: &AdapterInputs<'_>,
        symbol: &str,
        leverage: f64,
    ) -> Result<(), LeverageWriteError> {
        let creds = Self::credentials(inputs).ok_or_else(|| {
            LeverageWriteError::Refused("the account holds no bybit api key pair".to_string())
        })?;
        crate::execution::write_symbol_leverage(
            &creds,
            &endpoints_of(inputs).map_err(LeverageWriteError::Refused)?,
            symbol,
            leverage,
            attribution_code_from(inputs.secrets, VENUE),
        )
    }

    /// The symbol's position mode and the account's margin mode put right BEFORE the gatewayhub's probe
    /// reads them: the start's own rule ([`crate::execution::heal_symbol_modes`]) with THIS account's key pair,
    /// on the host its tier selects. No key pair, no call.
    fn heal_modes(&self, inputs: &AdapterInputs<'_>, symbol: &str) -> Result<(), String> {
        let creds = Self::credentials(inputs)
            .ok_or_else(|| "the account holds no bybit api key pair".to_string())?;
        crate::execution::heal_symbol_modes(
            &creds,
            &endpoints_of(inputs)?,
            symbol,
            attribution_code_from(inputs.secrets, VENUE),
        )
    }

    /// An operator's READ of the account (`crate::reconcile::venue_read`): its wallets' balances or every book's open orders,
    /// with THIS account's key pair on the host its tier selects ([`endpoints_of`]). It writes nothing. No key pair
    /// or no address row, nothing asked.
    fn venue_read(
        &self,
        inputs: &AdapterInputs<'_>,
        kind: vike_model::VenueReadKind,
    ) -> Result<vike_model::VenueRead, String> {
        let creds = Self::credentials(inputs).ok_or_else(|| {
            "the account holds no bybit api key pair; nothing was asked".to_string()
        })?;
        crate::reconcile::venue_read::venue_read(&creds, endpoints_of(inputs).map(|e| e.rest), kind)
    }
}

/// [`VenueAdapterBybit::credential_probe`]'s body, with the reconcile factory taken as a parameter:
/// the step that decides which HOST the probe's signed balance read is sent to. The probe passes
/// the real factory; a test passes a double that records the key and the network it is handed,
/// which is how `adapter_tests.rs` pins that the probe signs with the account's pair, on the host of
/// the tier it reports.
fn credential_probe_with(
    inputs: &AdapterInputs<'_>,
    build_reconcile: impl FnOnce(&Credentials, &str, &str) -> Option<Box<dyn ReconcileClient>>,
) -> Option<CredentialProbe> {
    // The tier's verdict, bound once for the host the balance read is signed against and the
    // tier reported beside it — the legacy probe's `probe_mainnet`, which for this venue was the
    // tier alone. A missing row: a probe that FAILS in the row's words (owner ruling B-19), so the
    // credential leg refuses the account loudly; no probe would be silence where the leg exists to fail.
    let creds = VenueAdapterBybit::credentials(inputs)?;
    let endpoints = match endpoints_of(inputs) {
        Ok(endpoints) => endpoints,
        Err(why) => return Some(CredentialProbe::failing(why)),
    };
    // No symbol (owner, 2026-11-13: none is compiled in, and the venue needs none here): the probe reads the
    // account-wide unified wallet alone, which a name that settles in no coin selects.
    let client = build_reconcile(&creds, "", &endpoints.rest)?;
    Some(CredentialProbe::RecordsIdentity {
        client,
        bound_tier: bound_tier(VenueNetwork::for_tier(inputs.tier)),
    })
}

/// [`crate::execution::rest::endpoints`] for the account `inputs` names, a refusal in its words.
fn endpoints_of(inputs: &AdapterInputs<'_>) -> Result<BybitUrls, String> {
    crate::execution::rest::endpoints(inputs.settings.urls(), inputs.tier)
        .map_err(|r| r.to_string())
}

#[path = "adapter_tests.rs"]
#[cfg(test)]
mod adapter_tests;
