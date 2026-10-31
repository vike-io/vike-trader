//! ig's VENUE ADAPTER — the `vike_bridge_core::venue_adapter::VenueAdapter` contract over the
//! session-auth REST exec client and its dedicated reconcile session
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md).
//!
//! Moved, behaviour unchanged, from `vike-tradehub-core`: the `("ig", _)` arm, its arming-probe row, its
//! book-identity row and its clock row WITH the read (the read `vike-tradehub-core` called `ig_time` is
//! [`IgVenueAdapter::server_time_ms`]).
//!
//! IG (IG Group) FX/CFD via session-auth REST. LIVE exec when an account below `live` holds its
//! `API_KEY` / `IDENTIFIER` / `PASSWORD` fields (`AdapterInputs::credentials`), else paper. An
//! EXEC-ONLY start: market data comes from a separate feed. `IgExecutionClient::spawn` is
//! INFALLIBLE at start time (an ExecActor + a background Lightstreamer stream that each self-gate
//! on their own login), so there is NO connect-failure paper-demotion branch: present creds ⇒ live,
//! absent ⇒ paper.
//!
//! ⚠ **A LIVE ACCOUNT'S KEY SET IS NAMED, NOT IGNORED.** This arm has no live arm: it starts only
//! the DEMO gateway. An account has ONE tier and its credentials are that tier's (decision 0156
//! item 6), so a `live` account holding a complete trio holds a set this arm will not use: it
//! answers [`PaperCause::LiveTierNotWired`] rather than `NoCredentials` (the words of an empty
//! account), and `start` says so at `error!`
//! (`vike_bridge_core::venue_adapter::report_live_tier_not_wired`). No venue starts live that did not
//! before. What the arm says about the live tier is ONE function
//! (`IgVenueAdapter::say_what_the_live_tier_is`), which also speaks for a labelled account that is
//! never started ([`VenueAdapter::report_not_running_account`]); an incomplete or empty account is
//! silent.

use std::time::Duration;

use vike_bridge_core::venue_adapter::{
    AdapterInputs, AdapterOutcome, AdapterRequest, BookIdentity, ClockAuth, ClockDecl, ClockRisk,
    DeclaredGridSource, ExecOutcome, HeldBelowLive, LiveExec, PaperCause, Resolution, VenueAdapter,
    VenueDeclaration, recon_if_enabled, report_live_tier_not_wired,
};
use vike_exec::recon::ReconClient;
use vike_exec::{EventSender, ExecutionClient};
use vike_model::AccountTier;

use crate::config::{DemoArm, FIELDS, IgConfig, demo_arm, load_ig_config_from, pin_of};
use crate::exec::stream::DealIds;
use crate::recon_client::VENUE;

/// ig's start. `vike_tradehub::registry::REGISTRY` holds `&IgVenueAdapter`.
pub struct IgVenueAdapter;

impl IgVenueAdapter {
    /// The one decision `resolve`, `start` and `report_not_running_account` act on: this account's
    /// trio at this account's tier (`crate::config`'s `demo_arm`).
    fn arm(inputs: &AdapterInputs<'_>) -> DemoArm {
        match demo_arm(inputs.tier, &inputs.credentials) {
            // The row's book rides the config, so every login of this start (exec, stream, reconcile, the
            // feeds it spawns) puts its session on the account the operator named (`crate::account_pick`).
            DemoArm::Demo(mut cfg) => {
                cfg.account_pin = pin_of(inputs);
                DemoArm::Demo(cfg)
            }
            other => other,
        }
    }

    /// **Everything this arm says about the LIVE tier**, in one place: `start` calls it for the
    /// account it is starting and [`VenueAdapter::report_not_running_account`] for a labelled account
    /// that is never started, so the two cannot word one fact two ways. Only a `live` account
    /// holding the whole trio is said; anything else is silent.
    fn say_what_the_live_tier_is(inputs: &AdapterInputs<'_>, arm: &DemoArm) {
        if matches!(arm, DemoArm::LiveTierNotWired) {
            report_live_tier_not_wired(VENUE, inputs.account, "demo");
        }
    }
}

/// [`IgVenueAdapter::start`]'s body, with its two NETWORK steps taken as parameters: `spawn` builds
/// the exec client (whose ExecActor and Lightstreamer stream each log in from their own thread) and
/// `connect_recon` is the dedicated reconcile login. `start` passes the real two; a test passes
/// offline doubles, which is what lets `adapter_tests.rs` read the fields THIS body sets — the bound
/// tier above all — and count the reconcile logins it attempts.
fn start_with(
    req: AdapterRequest<'_>,
    spawn: impl FnOnce(IgConfig, EventSender) -> Box<dyn ExecutionClient + Send>,
    connect_recon: impl FnOnce(&IgConfig, &str) -> Option<Box<dyn ReconClient>>,
) -> AdapterOutcome {
    // No usable trio ⇒ paper. A LIVE account's complete trio is paper too, and the one thing said:
    // why the venue does not run.
    let arm = IgVenueAdapter::arm(&req.inputs);
    IgVenueAdapter::say_what_the_live_tier_is(&req.inputs, &arm);
    let DemoArm::Demo(cfg) = arm else {
        return AdapterOutcome::paper();
    };
    tracing::warn!(
        venue = VENUE,
        account = %req.inputs.account,
        tier = AccountTier::Demo.as_str(),
        "ig: DEMO credentials present → LIVE exec client (real demo orders)"
    );
    // Reconcile handle: `crates/bridges/ig/src/recon_client.rs`'s `recon_client` logs in a FRESH
    // dedicated `IgSession`, isolated from BOTH the exec side's own session and its background
    // Lightstreamer login (IG permits concurrent sessions). Built from `&cfg` HERE, before `cfg`
    // moves into `spawn` below; the login is a blocking round trip at start time, and a failed one
    // (`None`) leaves the venue reconcile-inert with exec unaffected. Interval-only (no
    // `recon_trigger`). ORDER-COMPLETENESS: `/workingorders` is the CURRENT resting set, so a local
    // order absent from it is a genuine terminal (`OrphanLocalOrder`); the rollout exposure is
    // POSITION-side (`hybrid` auto-applies `PositionDrift`). ⚠ The coid-keyed order paths DO apply to
    // IG, for the orders THIS process placed: `start` hands the reconcile client the exec client's
    // `coid <-> dealId` map, so a live one is reported under its own coid. Without that, every
    // working order paired an `UnknownOrder` with an `OrphanLocalOrder` (a report's coid was always
    // `None`); an order from before a restart, or one whose `dealId` was never learned, still does.
    //
    // LAZY: with reconciliation off that dedicated login never happens.
    let recon = recon_if_enabled(req.recon_enabled, || connect_recon(&cfg, req.symbol));
    AdapterOutcome {
        exec: ExecOutcome::Live(LiveExec {
            client: spawn(cfg, req.events.clone()),
            // The tier the demo account's trio authenticates, which `vike-tradehub-core` records the
            // account against: the arm has no live tier.
            bound_tier: AccountTier::Demo,
            grid: None,
            contract_value: None,
            margin_mode: None,
            leg_grids: Vec::new(),
        }),
        recon,
        identity: None,
    }
}

impl VenueAdapter for IgVenueAdapter {
    fn venue(&self) -> &'static str {
        VENUE
    }

    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration {
            addresses_accounts: true,
            process_exclusive: None,
            takes_recon_trigger: false,
            // An exec-only arm with NO grid pre-fetch: the engine symbol keeps
            // `RiskLimits::new()`'s permissive `None`s, so a leg inherits nothing.
            grid_source: DeclaredGridSource::NoGrid,
            // `crates/bridges/ig/src/config.rs`'s `FIELDS`: the `IDENTIFIER` field is the LOGIN.
            // ⚠ It names the login rather than the dealing account, and IG can hold several
            // accounts behind one login — so this row is sound in the direction that matters (two
            // labels with one identifier ARE one login, and this daemon selects that login's
            // default account both times) and silent in the other (two different logins on one
            // underlying account cannot be seen from here). A warning that fires only on a
            // certainty is the contract; the missed case is a miss, not a false report.
            book_identity: BookIdentity::Named {
                name_fields: &["IDENTIFIER"],
                evm_key_fields: &[],
            },
            // The one CLOCK read on the roster that sends a credential, and it is cheap: the API
            // key alone, no `POST /session` login, no session to expire. The key is guaranteed
            // present whenever this row can fire — the clock leg runs only for a venue the arming
            // projection accepts, and IG's live gate IS that key. ⚠ That projection is
            // CEILING-AWARE: an ig capped to `paper` is never read at all.
            clock: ClockDecl::Wired {
                endpoint: "GET /session/encryptionKey (X-IG-API-KEY only)",
                auth: ClockAuth::Credentialed,
                risk: ClockRisk::NoTimestamp,
            },
        }
    }

    /// A complete trio on an account below `live` arms DEMO (held by `DemoOnlyArm`); a `live`
    /// account's complete trio is [`PaperCause::LiveTierNotWired`]; anything less is
    /// [`PaperCause::NoCredentials`].
    fn resolve(&self, inputs: &AdapterInputs<'_>) -> Resolution {
        match Self::arm(inputs) {
            DemoArm::Demo(_) => Resolution::Armed {
                tier: AccountTier::Demo,
                held_below_live: Some(HeldBelowLive::DemoOnlyArm),
            },
            DemoArm::LiveTierNotWired => Resolution::Paper(PaperCause::LiveTierNotWired),
            DemoArm::Unconfigured => Resolution::Paper(PaperCause::NoCredentials),
        }
    }

    fn start(&self, req: AdapterRequest<'_>) -> AdapterOutcome {
        // Read BEFORE `req` moves into `start_with`; the spawn closure hands it to the client.
        let halt_path = req.inputs.process.halt_path.clone();
        // ONE `coid <-> dealId` map for this start, handed to BOTH the exec client (which fills it
        // at submit and at the confirm watchdog) and the reconcile client (which reads it to name a
        // live working order's coid) — two maps would be the defect this replaces.
        let deal_ids = DealIds::shared();
        let recon_ids = deal_ids.clone();
        start_with(
            req,
            move |cfg, events| {
                Box::new(
                    crate::exec::IgExecutionClient::spawn_sharing(cfg, events, deal_ids)
                        .with_halt_path(halt_path),
                )
            },
            move |cfg, symbol| crate::recon_client::recon_client_sharing(cfg, symbol, recon_ids),
        )
    }

    /// A labelled account is started only when it armed, so a `live` one never reaches `start`;
    /// `vike-tradehub-core` asks for what `start` would have said (see the trait method).
    fn report_not_running_account(&self, inputs: &AdapterInputs<'_>) {
        Self::say_what_the_live_tier_is(inputs, &Self::arm(inputs));
    }

    /// `GET /session/encryptionKey` with the API key only — no login, no session. The gateway
    /// mirrors the start, which reaches the DEMO gateway and nothing else; the probed account's
    /// key, as `vike-tradehub-core`'s `ig_time` read it. The parse (and its fixture test) lives beside the
    /// request, in `crates/bridges/ig/src/rest.rs`'s `parse_server_time_ms`.
    fn server_time_ms(&self, inputs: &AdapterInputs<'_>, timeout: Duration) -> Result<i64, String> {
        let Some(cfg) = load_ig_config_from(AccountTier::Demo, &inputs.credentials) else {
            // The FIELDS the account lacks (the loader's own list, so they cannot rot against this
            // crate) — names only, never a value.
            let missing = inputs.credentials.missing(&FIELDS).join(", ");
            return Err(format!(
                "the account's {missing} is not set, so there is no key to read the clock with"
            ));
        };
        crate::rest::IgSession::server_time_ms(&cfg, timeout).map_err(|e| e.to_string())
    }
}

#[path = "adapter_tests.rs"]
#[cfg(test)]
mod adapter_tests;
