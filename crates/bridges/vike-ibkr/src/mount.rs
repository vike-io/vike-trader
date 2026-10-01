//! ibkr's VENUE MOUNT — the `vike_bridge_core::venue_mount::VenueMount` contract over the
//! socket/cpapi exec client and its dedicated cpapi reconcile client
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md).
//!
//! Moved, behaviour unchanged, from `vike-mount`: its `("ibkr", _)` arm (now
//! [`IbkrVenueMount::mount`]), its arming-probe row ([`IbkrVenueMount::resolve`]) and its clock,
//! book-identity and grid-source rows ([`IbkrVenueMount::declaration`]).
//!
//! ⚠ **Compiled only under this crate's `ibkr` umbrella** (`ibkr-socket` + `ibkr-cpapi`): the mount
//! reads BOTH backends — the cpapi reconcile client and the socket grid pre-fetch. A build without
//! the feature has no IBKR mount at all, and `vike-tradehub`'s registry says so with a
//! `FeatureAbsent` row: the venue mounts paper and the arming screen names the missing feature.
//!
//! IBKR uses `IBKR_{DEMO|LIVE}_{HOST|PORT|CLIENT_ID|ACCOUNT|BACKEND}` config, NOT the generic
//! `{VENUE}_DEMO_API_KEY` shape. ABSENT ACCOUNT ⇒ paper (absent credentials are the live gate). It
//! resolves the DEMO (paper-account) tier only; a LIVE flip is a deliberate follow-up (resolve
//! `IBKR_LIVE_*` first, with a REAL-MONEY warning), not configuration. The exec backend (socket by
//! default, or cpapi) is chosen inside [`crate::IbkrExecutionClient::connect`] by `cfg.backend`.
//!
//! ⚠ WEAKER ROBUSTNESS CONTRACT, the same as cTrader's: `connect` is a BLOCKING, FALLIBLE handshake
//! performed synchronously at mount (socket: TCP connect + API handshake to a running TWS/Gateway;
//! cpapi: a browser-authenticated Client Portal Gateway). A connect failure DEMOTES the venue to
//! PAPER for the whole session — there is no in-thread reconnect.

use vike_bridge_core::credentials::Environment;
use vike_bridge_core::venue_mount::{
    BookIdentity, ClockDecl, DeclaredGridSource, ExecOutcome, HeldBelowLive, LiveExec, MountInputs,
    MountOutcome, MountRequest, PaperCause, Resolution, Tier, VenueDeclaration, VenueMount,
    recon_if_enabled,
};
use vike_exec::recon::ReconClient;
use vike_exec::{EventSender, ExecutionClient};
use vike_model::SymbolProperties;

use crate::config::{IbkrConfig, load_ibkr_config_for_account};
use crate::error::IbkrError;
use crate::recon_client::{VENUE, recon_client};

/// ibkr's mount. `vike_tradehub::registry::REGISTRY` holds `&IbkrVenueMount` under that crate's
/// `ibkr` feature.
pub struct IbkrVenueMount;

impl IbkrVenueMount {
    /// The one gate `resolve` and `mount` share: this account's DEMO config — `_ACCOUNT` present
    /// and every optional key parseable — on the DEMO tier's gateway settings
    /// (`MountInputs::settings`, decision 0095).
    fn config(inputs: &MountInputs<'_>) -> Option<IbkrConfig> {
        load_ibkr_config_for_account(
            Environment::Demo,
            inputs.account,
            inputs.secrets,
            inputs.settings,
        )
    }
}

impl VenueMount for IbkrVenueMount {
    fn venue(&self) -> &'static str {
        VENUE
    }

    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration {
            addresses_accounts: true,
            process_exclusive: None,
            // Interval-only reconcile: `vike_mount::build_node` threads no trigger for this venue.
            takes_recon_trigger: false,
            // `crates/bridges/vike-ibkr/src/properties.rs`'s `fetch_ibkr_properties` opens its OWN
            // transient socket connection to TWS/Gateway per call (IBKR publishes no keyless grid
            // endpoint), and is socket-backend-only.
            grid_source: DeclaredGridSource::PerSymbolFetch,
            // `crates/bridges/vike-ibkr/src/config.rs`'s `load_ibkr_config_for_account`: `_ACCOUNT`
            // selects the `DU…`/`U…` account every order is placed in.
            book_identity: BookIdentity::Named {
                prefix: "IBKR",
                demo_tiers: &["DEMO"],
                live_tiers: &["LIVE"],
                name_suffixes: &["ACCOUNT"],
                evm_key_suffixes: &[],
            },
            // Neither backend offers a public clock: the socket API's time request rides the
            // authenticated TWS socket and the CP Gateway is a LOCAL process, so a "server time"
            // read there would largely be this host comparing itself against itself.
            clock: ClockDecl::NotWired {
                reason: "feature-gated here, and both backends put the clock behind an \
                         authenticated TWS socket / local CP-Gateway session this pre-mount step \
                         does not open",
                unmeasured_risk: None,
            },
        }
    }

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
        mount_with(
            req,
            recon_client,
            |cfg, events| {
                crate::IbkrExecutionClient::connect(cfg, events)
                    .map(|client| Box::new(client) as Box<dyn ExecutionClient + Send>)
            },
            crate::fetch_ibkr_properties,
        )
    }
}

/// [`IbkrVenueMount::mount`]'s body, with its three NETWORK steps as parameters — the cpapi
/// reconcile factory, the exec connect and the grid pre-fetch — so a test can reach the connected
/// branch and the connect-failure demotion without a Gateway. `mount` passes the real three. The
/// order is the legacy arm's: the reconcile client first, then the connect, then the grid, which
/// only a connected session fetches.
fn mount_with<R, C, G>(
    req: MountRequest<'_>,
    build_recon: R,
    connect: C,
    fetch_grid: G,
) -> MountOutcome
where
    R: FnOnce(&IbkrConfig, &str) -> Option<Box<dyn ReconClient>>,
    C: FnOnce(&IbkrConfig, EventSender) -> Result<Box<dyn ExecutionClient + Send>, IbkrError>,
    G: FnOnce(&IbkrConfig, &str) -> Option<SymbolProperties>,
{
    let Some(cfg) = IbkrVenueMount::config(&req.inputs) else { return MountOutcome::paper() };
    // Reconcile FIRST, on its OWN dedicated cpapi `IbkrReconClient`, so a reconcile report fetch
    // never contends the exec transport. It is cpapi-only whatever the exec backend, so with a
    // socket backend and no CP Gateway up it resolves `None` (unwired) and exec is unaffected.
    // LAZY: with reconciliation off the cpapi `tickle`/`secdef_search` handshake is never
    // performed. Built BEFORE the connect, so the demotion below drops it.
    let recon = recon_if_enabled(req.recon_enabled, || build_recon(&cfg, req.symbol));
    match connect(&cfg, req.events.clone()) {
        Ok(client) => {
            tracing::warn!(
                backend = ?cfg.backend,
                "ibkr: config present → LIVE exec client (real orders on the resolved Gateway account)"
            );
            // Live RiskGate from the venue's REAL grid: one blocking best-effort `contractDetails`
            // pre-fetch over a dedicated throwaway socket connection. On failure — unreachable
            // Gateway, unparseable symbol, empty reply, OR the cpapi backend (the fetch is
            // socket-only) — the permissive default stands.
            let grid = fetch_grid(&cfg, req.symbol);
            MountOutcome {
                exec: ExecOutcome::Live(LiveExec {
                    client,
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
        Err(e) => {
            // Demote to PAPER for this session. The reconcile client built moments ago would
            // reconcile a PAPER engine against LIVE state — drop it before the log line, the order
            // the legacy arm had, like every paper venue.
            drop(recon);
            tracing::warn!(
                error = %e,
                "ibkr: exec connect failed → falling back to PAPER for this session"
            );
            MountOutcome::paper()
        }
    }
}

#[path = "mount_tests.rs"]
#[cfg(test)]
mod mount_tests;
