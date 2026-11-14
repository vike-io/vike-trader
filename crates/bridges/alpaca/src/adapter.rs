//! alpaca's VENUE ADAPTER — the `vike_bridge_core::venue_adapter::VenueAdapter` contract over the
//! OAuth2 client-credentials exec client and its reconcile client
//! (`docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md`).
//!
//! Moved, behaviour unchanged, from `vike-tradehub-core`: its `("alpaca", _)` arm (now
//! [`VenueAdapterAlpaca::start`]), its arming-probe row ([`VenueAdapterAlpaca::resolve`]), its clock,
//! book-identity and grid-source rows ([`VenueAdapterAlpaca::declaration`]) and its startup
//! credential probe ([`VenueAdapterAlpaca::credential_probe`]).
//!
//! Alpaca uses an OAuth2 client-credentials pair plus a pinned account id (the started account's
//! `CLIENT_ID` / `CLIENT_SECRET` / `ACCOUNT_ID` fields, `AdapterInputs::credentials`), NOT the
//! generic `API_KEY` / `API_SECRET` shape, so it self-gates on `crate::config`'s `sandbox_arm`.
//! This is the EXEC + reconcile start: market data comes from the separate `DataClientBusSpecAlpaca` feed
//! the daemon wires elsewhere.
//!
//! Unlike ctrader, `crate::execution::spawn` is INFALLIBLE at start time — it spawns a
//! self-reconnecting `ExecutionClientActor` + SSE reader (no blocking startup handshake), so there is NO
//! connect-failure paper-demotion branch: present creds ⇒ live, absent ⇒ paper.
//!
//! ⚠ **A LIVE ACCOUNT'S KEY SET IS NAMED, NOT IGNORED.** This arm has no live arm: it starts only
//! the SANDBOX. An account has ONE tier and its credentials are that tier's (decision 0156 item 6),
//! so a `live` account holding a complete trio holds a set this arm will not use: it answers
//! `ArmingNotRunningCause::LiveTierNotWired` rather than `NoCredentials` (the words of an empty account),
//! and `start` says so at `error!` (`vike_bridge_core::venue_adapter::report_live_tier_not_wired`).
//! No venue starts live that did not before. What the arm says about the live tier is ONE function
//! (the shared `ArmingDemoOnly::report_live_tier`, in this venue's word "sandbox"), which also speaks for a
//! labelled account that is never started ([`VenueAdapter::report_not_running_account`]); an incomplete
//! or empty account is silent.

use std::sync::{Arc, Mutex};

use vike_bridge_core::venue_adapter::{
    AdapterInputs, AdapterOutcome, AdapterRequest, ArmingDemoOnly, ArmingResolution,
    BookIdentityRule, ClockDeclaration, CredentialProbe, VenueAdapter, VenueAdapterDeclaration,
    VenueAdapterExecutionOutcome, VenueAdapterExecutionRunning,
};
use vike_execution::recon::ReconcileClient;
use vike_execution::{EventSender, ExecutionClient};
use vike_model::{AccountTier, GridSource, SymbolProperties};
use vike_venues::venue_urls::UrlRefusal;

use crate::config::{AlpacaConfig, sandbox_arm};
use crate::feed::VENUE;

/// alpaca's start. `vike_tradehub::registry::REGISTRY` holds `&VenueAdapterAlpaca`.
pub struct VenueAdapterAlpaca;

impl VenueAdapterAlpaca {
    /// The one decision `resolve`, `start`, `report_not_running_account` and `credential_probe`
    /// share: this account's trio at this account's tier.
    fn arm(inputs: &AdapterInputs<'_>) -> ArmingDemoOnly<Result<AlpacaConfig, UrlRefusal>> {
        sandbox_arm(inputs.tier, &inputs.credentials, inputs.settings.urls())
    }
}

/// [`VenueAdapterAlpaca::start`]'s body, with its three NETWORK steps taken as parameters: `fetch_grid`
/// is the blocking `/v1/assets` pre-fetch, `build_reconcile` the reconcile client's factory and `spawn`
/// the exec client. `start` passes the real three; a test passes offline doubles, which is what lets
/// `adapter_tests.rs` run the branch that finds a sandbox key set (every other branch of this body is
/// already offline) — the only other way to reach it is to dial `broker-api.alpaca.markets`.
fn start_with(
    req: AdapterRequest<'_>,
    fetch_grid: impl FnOnce(&AlpacaConfig, &str) -> Option<SymbolProperties>,
    build_reconcile: impl FnOnce(&AlpacaConfig, &str) -> Option<Box<dyn ReconcileClient>>,
    spawn: impl FnOnce(AlpacaConfig, EventSender) -> Box<dyn ExecutionClient + Send>,
) -> AdapterOutcome {
    // No Alpaca creds ⇒ paper: `vike-tradehub-core` builds the paper client, and nothing reconciles. A
    // LIVE account's complete trio is paper too, and the one thing said: why the venue does not run. The
    // sandbox's rows are looked up before anything is dialled: a refused one is the reason, at `error!`.
    let Some(config) =
        VenueAdapterAlpaca::arm(&req.inputs).config_for_start(VENUE, req.inputs.account)
    else {
        return AdapterOutcome::paper();
    };
    tracing::warn!(
        venue = VENUE,
        account = %req.inputs.account,
        tier = AccountTier::Demo.as_str(),
        "alpaca: SANDBOX credentials present → LIVE exec client (real sandbox orders)"
    );
    // Live RiskGate from the venue's REAL grid (PR-2b): one blocking best-effort pre-fetch
    // (`/v1/assets`, a throwaway OAuth2 lifecycle isolated from the exec/recon clients). On
    // failure the permissive default stands, same as the crypto arms' failed pre-fetch.
    let grid = fetch_grid(&config, req.symbol);
    // Reconcile handle (audit A1 item 4): a FRESH Bearer `AlpacaTransport` (a SECOND OAuth2
    // client-credentials lifecycle) dedicated to reconcile reads, isolated from the exec
    // side's own. Built from `&config` BEFORE `config` moves into `spawn` below. Pure to construct,
    // so NOT gated on `recon_enabled` — exactly as the arm was.
    let recon = build_reconcile(&config, req.symbol);
    AdapterOutcome {
        exec: VenueAdapterExecutionOutcome::Running(VenueAdapterExecutionRunning {
            client: spawn(config, req.events.clone()),
            bound_tier: AccountTier::Demo,
            grid,
            contract_value: None,
            margin_mode: None,
            leg_grids: Vec::new(),
        }),
        reconcile: recon,
        identity: None,
    }
}

impl VenueAdapter for VenueAdapterAlpaca {
    fn venue(&self) -> &'static str {
        VENUE
    }

    fn declaration(&self) -> VenueAdapterDeclaration {
        VenueAdapterDeclaration {
            // `Self::arm` reads THIS account's fields (`AdapterInputs::credentials`).
            addresses_accounts: true,
            process_exclusive: None,
            // Interval-only reconcile: the arm never read `reconcile_trigger`.
            takes_reconcile_trigger: false,
            // `crates/bridges/alpaca/src/instrument.rs`'s `fetch_alpaca_properties` builds a
            // THROWAWAY OAuth2 client-credentials lifecycle per call — the most expensive
            // per-symbol source on the roster.
            grid_source: GridSource::PerSymbolFetch,
            // `crates/bridges/alpaca/src/config.rs`'s `load_alpaca_config_from`: the pinned
            // `ACCOUNT_ID` field scopes every Alpaca request, so it IS the book.
            book_identity: BookIdentityRule::Named {
                name_fields: &["ACCOUNT_ID"],
                evm_key_fields: &[],
            },
            // Alpaca's `GET /v1/clock` exists and reads well (+~110 ms, sub-second, stable over
            // three the CI box reps on 2026-08-08) but is HTTP 401 without the OAuth2
            // client-credentials Bearer minted by `crates/bridges/alpaca/src/transport/sign.rs`'s
            // `TokenSource`. Wiring it would put a second token exchange in front of a measurement
            // whose own risk row is `NoTimestamp` — alpaca's Bearer auth stamps nothing — so the
            // leg would cost a network lifecycle to catch a fault it cannot prevent.
            // ⚠ Its `timestamp` is RFC3339 with a NUMERIC US/Eastern offset (never `Z`), which
            // follows US DST: a parser that assumes UTC is wrong by four hours in August and five
            // in December.
            clock: ClockDeclaration::NotWired {
                reason: "its /v1/clock needs the OAuth2 client-credentials Bearer, i.e. a second \
                         token exchange before any measurement, and alpaca stamps no timestamp on \
                         a request",
                unmeasured_risk: None,
            },
        }
    }

    /// The arming-probe row: a complete trio on an account below `live` arms, and it arms DEMO —
    /// the arm hardcodes the sandbox tier, held there by `ArmingDemoOnly`. Anything less is paper: a
    /// `live` account's complete trio is `LiveTierNotWired`, and an incomplete or empty one is
    /// `NoCredentials` ([`ArmingDemoOnly::resolution`]).
    fn resolve(&self, inputs: &AdapterInputs<'_>) -> ArmingResolution {
        Self::arm(inputs).resolution()
    }

    fn start(&self, req: AdapterRequest<'_>) -> AdapterOutcome {
        // Read BEFORE `req` moves into `start_with`; the spawn closure hands it to the client.
        let halt_path = req.inputs.process.halt_path.clone();
        start_with(
            req,
            crate::instrument::fetch_alpaca_properties,
            crate::reconcile::recon_client,
            move |config, events| {
                Box::new(crate::execution::spawn(config, events).with_halt_path(halt_path))
            },
        )
    }

    /// A labelled account is started only when it armed, so a `live` one never reaches `start`;
    /// `vike-tradehub-core` asks for what `start` would have said (see the trait method).
    fn report_not_running_account(&self, inputs: &AdapterInputs<'_>) {
        Self::arm(inputs).report_live_tier(VENUE, inputs.account);
    }

    /// Was `vike-tradehub-core`'s `authed_read_probes` branch 2: the probed account's SANDBOX config
    /// (`Self::arm`, so a `live` account's trio is never sent to the sandbox hosts), PURE to
    /// construct (a fresh `TokenSource`/`AlpacaTransport`; the OAuth2 client-credentials mint happens on
    /// the first request). The error names BOTH hosts because they are different machines and
    /// either can be the one that is unreachable: the mint goes to `authx`, the balance read to
    /// `broker`.
    fn credential_probe(&self, inputs: &AdapterInputs<'_>) -> Option<CredentialProbe> {
        // A refused host row FAILS the leg in the refusal's words (owner ruling B-19: loud, never no probe).
        let config = match Self::arm(inputs) {
            ArmingDemoOnly::Demo(Ok(config)) => config,
            ArmingDemoOnly::Demo(Err(why)) => {
                return Some(CredentialProbe::failing(why.to_string()));
            }
            ArmingDemoOnly::LiveTierNotWired | ArmingDemoOnly::Unconfigured => return None,
        };
        // No symbol (owner, 2026-11-13: none is compiled in, and the venue needs none here): the probe reads the
        // account-wide balance alone, which no symbol scopes. ⚠ No MEASURED healthy reading accompanies this probe:
        // every Alpaca host was TCP-unreachable from the box that added it (the alpaca+ctrader live rehearsal, PR
        // #1407, Evidence 4), so it is proven by its FAILURE path and its offline tests. The venue's live wiring is
        // deferred by the owner; this path only compiles.
        let client = crate::reconcile::recon_client(&config, "")?;
        let authx = config.hosts.authx.clone();
        let broker = config.hosts.broker.clone();
        let client = Mutex::new(client);
        Some(CredentialProbe::ReadOnly(Arc::new(move || {
            let guard = client.lock().map_err(|_| "preflight probe lock poisoned".to_string())?;
            guard.fetch_balance().map(|_| ()).map_err(|e| {
                format!(
                    "{e} (OAuth2 token mint host {authx}, balance read host {broker}; \
                     credentials: the account's \
                     CLIENT_ID/CLIENT_SECRET/ACCOUNT_ID)"
                )
            })
        })))
    }

    /// The account's open orders, every symbol's (`crate::reconcile::venue_read`), on the sandbox this arm would start on;
    /// a `live` account is told so with nothing sent. The balance and the positions are not offered.
    fn venue_read(
        &self,
        inputs: &AdapterInputs<'_>,
        kind: vike_model::VenueReadKind,
    ) -> Result<vike_model::VenueRead, String> {
        if kind != vike_model::VenueReadKind::OpenOrders {
            return Err(vike_model::not_offered_words(VENUE, kind));
        }
        match Self::arm(inputs) {
            ArmingDemoOnly::Demo(Ok(config)) => crate::reconcile::venue_read::open_orders(&config),
            ArmingDemoOnly::Demo(Err(why)) => Err(format!("alpaca: {why}; nothing was asked")),
            ArmingDemoOnly::LiveTierNotWired => {
                Err("this bridge reaches only Alpaca's sandbox, so an account on the live tier \
                     has no host to read; nothing was asked"
                    .to_string())
            }
            ArmingDemoOnly::Unconfigured => {
                Err("the account holds no alpaca CLIENT_ID, CLIENT_SECRET and ACCOUNT_ID trio; \
                     nothing was asked"
                    .to_string())
            }
        }
    }
}

#[path = "adapter_tests.rs"]
#[cfg(test)]
mod adapter_tests;
