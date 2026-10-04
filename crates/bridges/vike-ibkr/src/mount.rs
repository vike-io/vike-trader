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
//! ⚠ **A LIVE-TIER ACCOUNT IS NAMED, NOT IGNORED.** The mount resolves the DEMO tier only. A store
//! holding the LIVE `_ACCOUNT` and no demo `_ACCOUNT` used to answer `NoCredentials` in silence — the
//! same words as an empty store. It now answers [`PaperCause::LiveTierNotWired`] and `mount` says so
//! at `error!` (`vike_bridge_core::venue_mount::report_unused_live_tier`). The live tier is
//! detected by that key alone (`crate::config::live_tier_account_alone`), not by the live tier's
//! gateway rows — and a demo account whose own gateway rows are refused is NOT this cause, because
//! the live account is not what keeps the venue on paper. Nothing else moves: no venue mounts live
//! that did not before. A live `_ACCOUNT` BESIDE the demo one still mounts the demo tier and now
//! says, once per process per account, that it is unused; a live tier written WITHOUT its
//! `_ACCOUNT` (a client id, a market-data type, and the account forgotten) is an `error!` naming the
//! key it lacks. What `mount` says about the live tier is ONE function over those states
//! (`IbkrVenueMount::say_what_the_live_tier_is`), and it speaks for a labelled account that is never
//! mounted ([`VenueMount::report_unmounted_account`]).
//!
//! ⚠ WEAKER ROBUSTNESS CONTRACT, the same as cTrader's: `connect` is a BLOCKING, FALLIBLE handshake
//! performed synchronously at mount (socket: TCP connect + API handshake to a running TWS/Gateway;
//! cpapi: a browser-authenticated Client Portal Gateway). A connect failure DEMOTES the venue to
//! PAPER for the whole session — there is no in-thread reconnect.

use vike_bridge_core::credentials::Environment;
use vike_bridge_core::venue_mount::{
    BookIdentity, ClockDecl, DeclaredGridSource, ExecOutcome, HeldBelowLive, LiveExec, LiveTierSet,
    MountInputs, MountOutcome, MountRequest, PaperCause, Resolution, Tier, VenueDeclaration,
    VenueMount, recon_if_enabled, report_unused_live_tier,
};
use vike_exec::recon::ReconClient;
use vike_exec::{EventSender, ExecutionClient};
use vike_model::SymbolProperties;

use crate::config::{
    IbkrConfig, live_tier_account_alone, live_tier_account_present, load_ibkr_config_for_account,
    tier_keys,
};
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

    /// Whether this account's store names a LIVE-tier `_ACCOUNT` and no demo one (see
    /// [`live_tier_account_alone`] for why those keys and not the loader). This arm never selects
    /// the live tier, so its only use is to name the cause [`PaperCause::LiveTierNotWired`] and to
    /// say so at `mount`.
    fn live_tier_present(inputs: &MountInputs<'_>) -> bool {
        live_tier_account_alone(inputs.account, inputs.secrets)
    }

    /// What the store holds of this account's LIVE tier. The one credential is `_ACCOUNT`, so the
    /// set is COMPLETE when that key is stored — alone (no demo `_ACCOUNT`, the cause `resolve`
    /// answers) when the demo tier did not mount, and simply present when it did (the "beside" case,
    /// which `resolve` has no cause for). Anything else with a live-tier key in it but no `_ACCOUNT`
    /// is half-written (`crate::config::tier_keys`).
    fn live_tier(inputs: &MountInputs<'_>, demo_mounts: bool) -> LiveTierSet {
        let complete = if demo_mounts {
            live_tier_account_present(inputs.account, inputs.secrets)
        } else {
            Self::live_tier_present(inputs)
        };
        LiveTierSet::read(complete, &tier_keys(Environment::Live, inputs.account), inputs.secrets)
    }

    /// **Everything this arm says about the LIVE tier**, in one place: `mount` calls it for the
    /// account it is mounting and [`VenueMount::report_unmounted_account`] for a labelled account
    /// that is never mounted, so the two cannot word one fact two ways. `demo_mounts` is whether the
    /// demo account loaded.
    fn say_what_the_live_tier_is(inputs: &MountInputs<'_>, demo_mounts: bool) {
        report_unused_live_tier(
            VENUE,
            inputs.account,
            "paper-account",
            demo_mounts,
            &Self::live_tier(inputs, demo_mounts),
        );
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
        } else if Self::live_tier_present(inputs) {
            Resolution::Paper(PaperCause::LiveTierNotWired)
        } else {
            Resolution::Paper(PaperCause::NoCredentials)
        }
    }

    fn mount(&self, req: MountRequest<'_>) -> MountOutcome {
        // Read BEFORE `req` moves into `mount_with`; the connect closure hands it to the client.
        let halt_path = req.inputs.process.halt_path.clone();
        mount_with(
            req,
            recon_client,
            move |cfg, events| {
                crate::IbkrExecutionClient::connect(cfg, events).map(|client| {
                    Box::new(client.with_halt_path(halt_path)) as Box<dyn ExecutionClient + Send>
                })
            },
            crate::fetch_ibkr_properties,
        )
    }

    /// A labelled account is mounted only when it armed, so one with no demo account never reaches
    /// `mount`; `vike-mount` asks for what `mount` would have said (see the trait method).
    fn report_unmounted_account(&self, inputs: &MountInputs<'_>) {
        Self::say_what_the_live_tier_is(inputs, Self::config(inputs).is_some());
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
    // A LIVE-tier account is the one thing about the store that is said either way: alone (or half
    // written) it is why the venue stays paper, beside the demo account it is why nothing changed.
    let cfg = IbkrVenueMount::config(&req.inputs);
    IbkrVenueMount::say_what_the_live_tier_is(&req.inputs, cfg.is_some());
    let Some(cfg) = cfg else {
        return MountOutcome::paper();
    };
    // Reconcile FIRST, on its OWN dedicated cpapi `IbkrReconClient`, so a reconcile report fetch
    // never contends the exec transport. It is cpapi-only whatever the exec backend, so with a
    // socket backend and no CP Gateway up it resolves `None` (unwired) and exec is unaffected.
    // LAZY: with reconciliation off the cpapi `tickle`/`secdef_search` handshake is never
    // performed. Built BEFORE the connect, so the demotion below drops it.
    let recon = recon_if_enabled(req.recon_enabled, || build_recon(&cfg, req.symbol));
    match connect(&cfg, req.events.clone()) {
        Ok(client) => {
            tracing::warn!(
                venue = VENUE,
                account = %req.inputs.account,
                tier = Tier::Demo.as_str(),
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
