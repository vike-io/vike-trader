//! alpaca's VENUE ADAPTER — the `vike_bridge_core::venue_adapter::VenueAdapter` contract over the
//! OAuth2 client-credentials exec client and its reconcile client
//! (`docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md`).
//!
//! Moved, behaviour unchanged, from `vike-tradehub-core`: its `("alpaca", _)` arm (now
//! [`AlpacaVenueAdapter::start`]), its arming-probe row ([`AlpacaVenueAdapter::resolve`]), its clock,
//! book-identity and grid-source rows ([`AlpacaVenueAdapter::declaration`]) and its startup
//! credential probe ([`AlpacaVenueAdapter::credential_probe`]).
//!
//! Alpaca uses an OAuth2 client-credentials pair plus a pinned account id (the started account's
//! `CLIENT_ID` / `CLIENT_SECRET` / `ACCOUNT_ID` fields, `AdapterInputs::credentials`), NOT the
//! generic `API_KEY` / `API_SECRET` shape, so it self-gates on `crate::config`'s `sandbox_arm`.
//! This is the EXEC + reconcile start: market data comes from the separate `AlpacaDataClient` feed
//! the daemon wires elsewhere.
//!
//! Unlike ctrader, `AlpacaExecutionClient::spawn` is INFALLIBLE at start time — it spawns a
//! self-reconnecting `ExecActor` + SSE reader (no blocking startup handshake), so there is NO
//! connect-failure paper-demotion branch: present creds ⇒ live, absent ⇒ paper.
//!
//! ⚠ **A LIVE ACCOUNT'S KEY SET IS NAMED, NOT IGNORED.** This arm has no live arm: it starts only
//! the SANDBOX. An account has ONE tier and its credentials are that tier's (decision 0156 item 6),
//! so a `live` account holding a complete trio holds a set this arm will not use: it answers
//! [`PaperCause::LiveTierNotWired`] rather than `NoCredentials` (the words of an empty account),
//! and `start` says so at `error!` (`vike_bridge_core::venue_adapter::report_live_tier_not_wired`).
//! No venue starts live that did not before. What the arm says about the live tier is ONE function
//! (`AlpacaVenueAdapter::say_what_the_live_tier_is`), which also speaks for a labelled account that
//! is never started ([`VenueAdapter::report_not_running_account`]); an incomplete or empty account is
//! silent.

use std::sync::{Arc, Mutex};

use vike_bridge_core::venue_adapter::{
    AdapterInputs, AdapterOutcome, AdapterRequest, BookIdentity, ClockDecl, CredentialProbe,
    DeclaredGridSource, ExecOutcome, HeldBelowLive, LiveExec, PaperCause, Resolution, VenueAdapter,
    VenueDeclaration, report_live_tier_not_wired,
};
use vike_exec::recon::ReconClient;
use vike_exec::{EventSender, ExecutionClient};
use vike_model::{AccountTier, SymbolProperties};

use crate::config::{AlpacaConfig, SandboxArm, sandbox_arm};
use crate::data::VENUE;

/// The symbol the startup credential probe scopes its reconcile client to — was `vike-tradehub-core`'s
/// `INLINE_AUTHED_READ_MARKETS` row.
/// It is this venue's row of `crates/vike-venues/src/venue_wiring.rs`'s `VENUE_WIRING` — the
/// symbol the daemon's default engine starts on — read at compile time, so the probe cannot drift
/// from the engine market and a row that loses its engine fails this crate's build. It scopes the recon client's order/position reads; the balance read the
/// probe issues is account-wide.
///
/// ⚠ **No MEASURED healthy reading accompanies this probe, and that is a statement, not an
/// omission.** It could not be measured healthy from the box that added it: every Alpaca host is
/// TCP-unreachable from the Windows dev box (measured — the alpaca+ctrader live rehearsal
/// (PR #1407), Evidence 4: DNS resolves, TCP 443 times out on BOTH tiers, while binance and github
/// answer 200). So it is proven by its FAILURE path and by its offline tests; a PASS remains
/// unwitnessed there and wants a run from a box with the egress.
const PROBE_SYMBOL: &str = match vike_venues::venue_wiring::wired_symbol(VENUE) {
    Some(symbol) => symbol,
    None => {
        panic!("this venue's credential probe needs an engine row in vike_model's VENUE_WIRING")
    }
};

/// alpaca's start. `vike_tradehub::registry::REGISTRY` holds `&AlpacaVenueAdapter`.
pub struct AlpacaVenueAdapter;

impl AlpacaVenueAdapter {
    /// The one decision `resolve`, `start`, `report_not_running_account` and `credential_probe`
    /// share: this account's trio at this account's tier.
    fn arm(inputs: &AdapterInputs<'_>) -> SandboxArm {
        sandbox_arm(inputs.tier, &inputs.credentials)
    }

    /// **Everything this arm says about the LIVE tier**, in one place: `start` calls it for the
    /// account it is starting and [`VenueAdapter::report_not_running_account`] for a labelled account
    /// that is never started, so the two cannot word one fact two ways. Only a `live` account
    /// holding the whole trio is said; anything else is silent.
    fn say_what_the_live_tier_is(inputs: &AdapterInputs<'_>, arm: &SandboxArm) {
        if matches!(arm, SandboxArm::LiveTierNotWired) {
            report_live_tier_not_wired(VENUE, inputs.account, "sandbox");
        }
    }
}

/// [`AlpacaVenueAdapter::start`]'s body, with its three NETWORK steps taken as parameters: `fetch_grid`
/// is the blocking `/v1/assets` pre-fetch, `build_recon` the reconcile client's factory and `spawn`
/// the exec client. `start` passes the real three; a test passes offline doubles, which is what lets
/// `adapter_tests.rs` run the branch that finds a sandbox key set (every other branch of this body is
/// already offline) — the only other way to reach it is to dial `broker-api.alpaca.markets`.
fn start_with(
    req: AdapterRequest<'_>,
    fetch_grid: impl FnOnce(&AlpacaConfig, &str) -> Option<SymbolProperties>,
    build_recon: impl FnOnce(&AlpacaConfig, &str) -> Option<Box<dyn ReconClient>>,
    spawn: impl FnOnce(AlpacaConfig, EventSender) -> Box<dyn ExecutionClient + Send>,
) -> AdapterOutcome {
    // No Alpaca creds ⇒ paper: `vike-tradehub-core` builds the paper client, and nothing reconciles. A
    // LIVE account's complete trio is paper too, and the one thing said: why the venue does not run.
    let arm = AlpacaVenueAdapter::arm(&req.inputs);
    AlpacaVenueAdapter::say_what_the_live_tier_is(&req.inputs, &arm);
    let SandboxArm::Sandbox(cfg) = arm else {
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
    let grid = fetch_grid(&cfg, req.symbol);
    // Reconcile handle (audit A1 item 4): a FRESH Bearer `AlpacaRest` (a SECOND OAuth2
    // client-credentials lifecycle) dedicated to reconcile reads, isolated from the exec
    // side's own. Built from `&cfg` BEFORE `cfg` moves into `spawn` below. Pure to construct,
    // so NOT gated on `recon_enabled` — exactly as the arm was.
    let recon = build_recon(&cfg, req.symbol);
    AdapterOutcome {
        exec: ExecOutcome::Live(LiveExec {
            client: spawn(cfg, req.events.clone()),
            bound_tier: AccountTier::Demo,
            grid,
            contract_size: None,
            margin_mode: None,
            leg_grids: Vec::new(),
        }),
        recon,
        identity: None,
    }
}

impl VenueAdapter for AlpacaVenueAdapter {
    fn venue(&self) -> &'static str {
        VENUE
    }

    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration {
            // `Self::arm` reads THIS account's fields (`AdapterInputs::credentials`).
            addresses_accounts: true,
            process_exclusive: None,
            // Interval-only reconcile: the arm never read `recon_trigger`.
            takes_recon_trigger: false,
            // `crates/bridges/alpaca/src/instruments.rs`'s `fetch_alpaca_properties` builds a
            // THROWAWAY OAuth2 client-credentials lifecycle per call — the most expensive
            // per-symbol source on the roster.
            grid_source: DeclaredGridSource::PerSymbolFetch,
            // `crates/bridges/alpaca/src/config.rs`'s `load_alpaca_config_from`: the pinned
            // `ACCOUNT_ID` field scopes every Alpaca request, so it IS the book.
            book_identity: BookIdentity::Named {
                name_fields: &["ACCOUNT_ID"],
                evm_key_fields: &[],
            },
            // Alpaca's `GET /v1/clock` exists and reads well (+~110 ms, sub-second, stable over
            // three the CI box reps on 2026-08-08) but is HTTP 401 without the OAuth2
            // client-credentials Bearer minted by `crates/bridges/alpaca/src/auth.rs`'s
            // `TokenSource`. Wiring it would put a second token exchange in front of a measurement
            // whose own risk row is `NoTimestamp` — alpaca's Bearer auth stamps nothing — so the
            // leg would cost a network lifecycle to catch a fault it cannot prevent.
            // ⚠ Its `timestamp` is RFC3339 with a NUMERIC US/Eastern offset (never `Z`), which
            // follows US DST: a parser that assumes UTC is wrong by four hours in August and five
            // in December.
            clock: ClockDecl::NotWired {
                reason: "its /v1/clock needs the OAuth2 client-credentials Bearer, i.e. a second \
                         token exchange before any measurement, and alpaca stamps no timestamp on \
                         a request",
                unmeasured_risk: None,
            },
        }
    }

    /// The arming-probe row: a complete trio on an account below `live` arms, and it arms DEMO —
    /// the arm hardcodes the sandbox tier, held there by `DemoOnlyArm`. Anything less is paper: a
    /// `live` account's complete trio is [`PaperCause::LiveTierNotWired`], and an incomplete or
    /// empty one is [`PaperCause::NoCredentials`].
    fn resolve(&self, inputs: &AdapterInputs<'_>) -> Resolution {
        match Self::arm(inputs) {
            SandboxArm::Sandbox(_) => Resolution::Armed {
                tier: AccountTier::Demo,
                held_below_live: Some(HeldBelowLive::DemoOnlyArm),
            },
            SandboxArm::LiveTierNotWired => Resolution::Paper(PaperCause::LiveTierNotWired),
            SandboxArm::Unconfigured => Resolution::Paper(PaperCause::NoCredentials),
        }
    }

    fn start(&self, req: AdapterRequest<'_>) -> AdapterOutcome {
        // Read BEFORE `req` moves into `start_with`; the spawn closure hands it to the client.
        let halt_path = req.inputs.process.halt_path.clone();
        start_with(
            req,
            crate::instruments::fetch_alpaca_properties,
            crate::recon_client::recon_client,
            move |cfg, events| {
                Box::new(
                    crate::exec::AlpacaExecutionClient::spawn(cfg, events)
                        .with_halt_path(halt_path),
                )
            },
        )
    }

    /// A labelled account is started only when it armed, so a `live` one never reaches `start`;
    /// `vike-tradehub-core` asks for what `start` would have said (see the trait method).
    fn report_not_running_account(&self, inputs: &AdapterInputs<'_>) {
        Self::say_what_the_live_tier_is(inputs, &Self::arm(inputs));
    }

    /// Was `vike-tradehub-core`'s `authed_read_probes` branch 2: the probed account's SANDBOX config
    /// (`Self::arm`, so a `live` account's trio is never sent to the sandbox hosts), PURE to
    /// construct (a fresh `TokenSource`/`AlpacaRest`; the OAuth2 client-credentials mint happens on
    /// the first request). The error names BOTH hosts because they are different machines and
    /// either can be the one that is unreachable: the mint goes to `authx`, the balance read to
    /// `broker`.
    fn credential_probe(&self, inputs: &AdapterInputs<'_>) -> Option<CredentialProbe> {
        let SandboxArm::Sandbox(cfg) = Self::arm(inputs) else { return None };
        let client = crate::recon_client::recon_client(&cfg, PROBE_SYMBOL)?;
        let authx = cfg.hosts.authx.to_string();
        let broker = cfg.hosts.broker.to_string();
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
}

#[path = "adapter_tests.rs"]
#[cfg(test)]
mod adapter_tests;
