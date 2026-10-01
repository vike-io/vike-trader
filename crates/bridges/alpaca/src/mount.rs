//! alpaca's VENUE MOUNT — the `vike_bridge_core::venue_mount::VenueMount` contract over the
//! OAuth2 client-credentials exec client and its reconcile client
//! (`docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md`).
//!
//! Moved, behaviour unchanged, from `vike-mount`: its `("alpaca", _)` arm (now
//! [`AlpacaVenueMount::mount`]), its arming-probe row ([`AlpacaVenueMount::resolve`]), its clock,
//! book-identity and grid-source rows ([`AlpacaVenueMount::declaration`]) and its startup
//! credential probe ([`AlpacaVenueMount::credential_probe`]).
//!
//! Alpaca uses an OAuth2 client-credentials pair (`ALPACA_SANDBOX_{CLIENT_ID,CLIENT_SECRET,
//! ACCOUNT_ID}`), NOT the generic `{VENUE}_DEMO_API_KEY` shape, so it self-gates on
//! `load_alpaca_config_for_account(Demo, …)`. This is the EXEC + reconcile mount: market data
//! comes from the separate `AlpacaDataClient` feed the daemon wires elsewhere.
//!
//! Unlike ctrader, `AlpacaExecutionClient::spawn` is INFALLIBLE at mount time — it spawns a
//! self-reconnecting `ExecActor` + SSE reader (no blocking startup handshake), so there is NO
//! connect-failure paper-demotion branch: present creds ⇒ live, absent ⇒ paper.

use std::sync::{Arc, Mutex};

use vike_bridge_core::credentials::Environment;
use vike_bridge_core::venue_mount::{
    BookIdentity, ClockDecl, CredentialProbe, DeclaredGridSource, ExecOutcome, HeldBelowLive,
    LiveExec, MountInputs, MountOutcome, MountRequest, PaperCause, Resolution, Tier,
    VenueDeclaration, VenueMount,
};

use crate::config::{AlpacaConfig, load_alpaca_config_for_account, load_alpaca_config_from};

/// The symbol the startup credential probe scopes its reconcile client to — was `vike-mount`'s
/// `INLINE_AUTHED_READ_MARKETS` row. It mirrors `crates/vike-tradehub/src/wired_markets.rs`'s `ALPACA_MARKET`
/// (the alpaca row of `WIRED_MARKETS`), as that row did: a bridge cannot name `vike-tradehub`, so the
/// two change together. It scopes the recon client's order/position reads; the balance read the
/// probe issues is account-wide.
///
/// ⚠ **No MEASURED healthy reading accompanies this probe, and that is a statement, not an
/// omission.** It could not be measured healthy from the box that added it: every Alpaca host is
/// TCP-unreachable from the Windows dev box (measured — the alpaca+ctrader live rehearsal
/// (PR #1407), Evidence 4: DNS resolves, TCP 443 times out on BOTH tiers, while binance and github
/// answer 200). So it is proven by its FAILURE path and by its offline tests; a PASS remains
/// unwitnessed there and wants a run from a box with the egress.
const PROBE_SYMBOL: &str = "AAPL";

/// alpaca's mount. `vike_tradehub::registry::REGISTRY` holds `&AlpacaVenueMount`.
pub struct AlpacaVenueMount;

impl AlpacaVenueMount {
    /// The one gate `resolve` and `mount` share: this account's SANDBOX key set.
    fn config(inputs: &MountInputs<'_>) -> Option<AlpacaConfig> {
        load_alpaca_config_for_account(Environment::Demo, inputs.account, inputs.secrets)
    }
}

impl VenueMount for AlpacaVenueMount {
    fn venue(&self) -> &'static str {
        crate::data::VENUE
    }

    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration {
            // `Self::config` reads THIS account's key names (`load_alpaca_config_for_account`).
            addresses_accounts: true,
            process_exclusive: None,
            // Interval-only reconcile: the arm never read `recon_trigger`.
            takes_recon_trigger: false,
            // `crates/bridges/alpaca/src/instruments.rs`'s `fetch_alpaca_properties` builds a
            // THROWAWAY OAuth2 client-credentials lifecycle per call — the most expensive
            // per-symbol source on the roster.
            grid_source: DeclaredGridSource::PerSymbolFetch,
            // `crates/bridges/alpaca/src/config.rs`'s `load_alpaca_config_for_account`, whose demo
            // tier is spelled `SANDBOX` (`alpaca_tier`) rather than `DEMO`.
            book_identity: BookIdentity::Named {
                prefix: "ALPACA",
                demo_tiers: &["SANDBOX"],
                live_tiers: &["LIVE"],
                name_suffixes: &["ACCOUNT_ID"],
                evm_key_suffixes: &[],
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

    /// The arming-probe row, unchanged: a complete SANDBOX key set for THIS account arms, and it
    /// arms DEMO — the arm hardcodes the sandbox tier, so under a `live` ceiling it is held there
    /// by `DemoOnlyArm`. Anything less is paper.
    fn resolve(&self, inputs: &MountInputs<'_>) -> Resolution {
        if Self::config(inputs).is_some() {
            Resolution::Armed {
                tier: Tier::Demo,
                held_below_live: Some(HeldBelowLive::DemoOnlyArm),
            }
        } else {
            Resolution::Paper(PaperCause::NoCredentials)
        }
    }

    fn mount(&self, req: MountRequest<'_>) -> MountOutcome {
        // No Alpaca creds ⇒ paper: `vike-mount` builds the paper client, and nothing reconciles.
        let Some(cfg) = Self::config(&req.inputs) else { return MountOutcome::paper() };
        tracing::warn!(
            "alpaca: SANDBOX credentials present → LIVE exec client (real sandbox orders)"
        );
        // Live RiskGate from the venue's REAL grid (PR-2b): one blocking best-effort pre-fetch
        // (`/v1/assets`, a throwaway OAuth2 lifecycle isolated from the exec/recon clients). On
        // failure the permissive default stands, same as the crypto arms' failed pre-fetch.
        let grid = crate::instruments::fetch_alpaca_properties(&cfg, req.symbol);
        // Reconcile handle (audit A1 item 4): a FRESH Bearer `AlpacaRest` (a SECOND OAuth2
        // client-credentials lifecycle) dedicated to reconcile reads, isolated from the exec
        // side's own. Built from `&cfg` BEFORE `cfg` moves into `spawn` below. Pure to construct,
        // so NOT gated on `recon_enabled` — exactly as the arm was.
        let recon = crate::recon_client::recon_client(&cfg, req.symbol);
        MountOutcome {
            exec: ExecOutcome::Live(LiveExec {
                client: Box::new(
                    crate::exec::AlpacaExecutionClient::spawn(cfg, req.events.clone())
                        .with_halt_path(req.inputs.process.halt_path.clone()),
                ),
                bound_tier: Tier::Demo,
                grid,
                contract_size: None,
                margin_mode: None,
                leg_grids: Vec::new(),
            }),
            recon,
            identity: None,
        }
    }

    /// Was `vike-mount`'s `authed_read_probes` branch 2, unchanged: the DEFAULT account's keys
    /// (`load_alpaca_config_from`), PURE to construct (a fresh `TokenSource`/`AlpacaRest`; the
    /// OAuth2 client-credentials mint happens on the first request). The error names BOTH hosts
    /// because they are different machines and either can be the one that is unreachable: the
    /// mint goes to `authx`, the balance read to `broker`.
    fn credential_probe(&self, inputs: &MountInputs<'_>) -> Option<CredentialProbe> {
        let cfg = load_alpaca_config_from(Environment::Demo, inputs.secrets)?;
        let client = crate::recon_client::recon_client(&cfg, PROBE_SYMBOL)?;
        let authx = cfg.hosts.authx.to_string();
        let broker = cfg.hosts.broker.to_string();
        let client = Mutex::new(client);
        Some(CredentialProbe::ReadOnly(Arc::new(move || {
            let guard = client.lock().map_err(|_| "preflight probe lock poisoned".to_string())?;
            guard.fetch_balance().map(|_| ()).map_err(|e| {
                format!(
                    "{e} (OAuth2 token mint host {authx}, balance read host {broker}; \
                     credentials \
                     ALPACA_SANDBOX_CLIENT_ID/_CLIENT_SECRET/_ACCOUNT_ID)"
                )
            })
        })))
    }
}

#[path = "mount_tests.rs"]
#[cfg(test)]
mod mount_tests;
