//! ig's VENUE MOUNT — the `vike_bridge_core::venue_mount::VenueMount` contract over the
//! session-auth REST exec client and its dedicated reconcile session
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md).
//!
//! Moved, behaviour unchanged, from `vike-mount`: the `("ig", _)` arm, its arming-probe row, its
//! book-identity row and its clock row WITH the read (the read `vike-mount` called `ig_time` is
//! [`IgVenueMount::server_time_ms`]).
//!
//! IG (IG Group) FX/CFD via session-auth REST. LIVE exec when the `IG_DEMO_*` config
//! (`IG_DEMO_API_KEY`/`_IDENTIFIER`/`_PASSWORD`) is present, else paper. An EXEC-ONLY mount:
//! market data comes from a separate feed. `IgExecutionClient::spawn` is INFALLIBLE at mount time
//! (an ExecActor + a background Lightstreamer stream that each self-gate on their own login), so
//! there is NO connect-failure paper-demotion branch: present creds ⇒ live, absent ⇒ paper.
//!
//! ⚠ **A LIVE-TIER KEY SET IS NAMED, NOT IGNORED.** This arm has no live arm: it mounts the DEMO
//! trio whatever the ceiling says. A store holding a COMPLETE `IG_LIVE_*` (or legacy `IG_MAINNET_*`)
//! trio and no demo one used to answer `NoCredentials` in silence — the same words as an empty
//! store. It now answers [`PaperCause::LiveTierNotWired`] and `mount` says so at `error!`
//! (`vike_bridge_core::venue_mount::report_unused_live_tier`). Nothing else moves: a live trio
//! BESIDE a demo one still mounts the demo tier, and no venue mounts live that did not before. What
//! `mount` says about the live tier is ONE function over the states the store can be in
//! (`IgVenueMount::say_what_the_live_tier_is`): a complete live trio beside the demo one is a
//! `warn!` that it is unused, once per process per account; a complete one alone is the `error!`
//! above; a HALF-written one (a typo'd secret name) is an `error!` naming the keys it lacks. The
//! same function speaks for a labelled account that is never mounted
//! ([`VenueMount::report_unmounted_account`]).

use std::time::Duration;

use vike_bridge_core::credentials::Environment;
use vike_bridge_core::venue_mount::{
    BookIdentity, ClockAuth, ClockDecl, ClockRisk, DeclaredGridSource, ExecOutcome, HeldBelowLive,
    LiveExec, LiveTierSet, MountInputs, MountOutcome, MountRequest, PaperCause, Resolution, Tier,
    VenueDeclaration, VenueMount, recon_if_enabled, report_unused_live_tier,
};
use vike_exec::recon::ReconClient;
use vike_exec::{EventSender, ExecutionClient};

use crate::config::{
    IgConfig, ig_env_var_names, load_ig_config_for_account, load_ig_config_from, tier_keys,
};
use crate::recon_client::VENUE;

/// ig's mount. `vike_tradehub::registry::REGISTRY` holds `&IgVenueMount`.
pub struct IgVenueMount;

impl IgVenueMount {
    /// The one credential resolution `resolve` and `mount` both act on: the NAMED account's
    /// `IG_DEMO_*` trio, and nothing else — the arm resolves `Environment::Demo` whatever the
    /// ceiling says.
    fn config(inputs: &MountInputs<'_>) -> Option<IgConfig> {
        load_ig_config_for_account(Environment::Demo, inputs.account, inputs.secrets)
    }

    /// Whether this account's store holds a COMPLETE LIVE-tier trio (the legacy `MAINNET` tier
    /// included — `load_ig_config_for_account` honours it). This arm never selects it, so its only
    /// use is to name the cause [`PaperCause::LiveTierNotWired`] and to say so at `mount`.
    fn live_tier_present(inputs: &MountInputs<'_>) -> bool {
        load_ig_config_for_account(Environment::Live, inputs.account, inputs.secrets).is_some()
    }

    /// What the store holds of this account's LIVE tier: complete (the loader's own verdict), a
    /// half-written trio (the names this arm's loader reads, current tier and legacy `MAINNET`
    /// alike — `crate::config::tier_keys`), or nothing.
    fn live_tier(inputs: &MountInputs<'_>) -> LiveTierSet {
        LiveTierSet::read(
            Self::live_tier_present(inputs),
            &tier_keys(Environment::Live, inputs.account),
            inputs.secrets,
        )
    }

    /// **Everything this arm says about the LIVE tier**, in one place: `mount` calls it for the
    /// account it is mounting and [`VenueMount::report_unmounted_account`] for a labelled account
    /// that is never mounted, so the two cannot word one fact two ways. `demo_mounts` is whether the
    /// demo trio loaded.
    fn say_what_the_live_tier_is(inputs: &MountInputs<'_>, demo_mounts: bool) {
        report_unused_live_tier(
            VENUE,
            inputs.account,
            "demo",
            demo_mounts,
            &Self::live_tier(inputs),
        );
    }
}

/// [`IgVenueMount::mount`]'s body, with its two NETWORK steps taken as parameters: `spawn` builds
/// the exec client (whose ExecActor and Lightstreamer stream each log in from their own thread) and
/// `connect_recon` is the dedicated reconcile login. `mount` passes the real two; a test passes
/// offline doubles, which is what lets `mount_tests.rs` read the fields THIS body sets — the bound
/// tier above all — and count the reconcile logins it attempts.
fn mount_with(
    req: MountRequest<'_>,
    spawn: impl FnOnce(IgConfig, EventSender) -> Box<dyn ExecutionClient + Send>,
    connect_recon: impl FnOnce(&IgConfig, &str) -> Option<Box<dyn ReconClient>>,
) -> MountOutcome {
    // A LIVE-tier trio is the one thing about the store that is said either way: alone (or half
    // written) it is why the venue stays paper, beside the demo trio it is why nothing changed.
    let cfg = IgVenueMount::config(&req.inputs);
    IgVenueMount::say_what_the_live_tier_is(&req.inputs, cfg.is_some());
    let Some(cfg) = cfg else {
        return MountOutcome::paper();
    };
    tracing::warn!(
        venue = VENUE,
        account = %req.inputs.account,
        tier = Tier::Demo.as_str(),
        "ig: DEMO credentials present → LIVE exec client (real demo orders)"
    );
    // Reconcile handle: `crates/bridges/ig/src/recon_client.rs`'s `recon_client` logs in a FRESH
    // dedicated `IgSession`, isolated from BOTH the exec side's own session and its background
    // Lightstreamer login (IG permits concurrent sessions). Built from `&cfg` HERE, before `cfg`
    // moves into `spawn` below; the login is a blocking round trip at mount time, and a failed one
    // (`None`) leaves the venue reconcile-inert with exec unaffected. Interval-only (no
    // `recon_trigger`). ORDER-COMPLETENESS: `/workingorders` is the CURRENT resting set, so a local
    // order absent from it is a genuine terminal; the rollout exposure is POSITION-side (`hybrid`
    // auto-applies `PositionDrift`).
    //
    // LAZY: with reconciliation off that dedicated login never happens.
    let recon = recon_if_enabled(req.recon_enabled, || connect_recon(&cfg, req.symbol));
    MountOutcome {
        exec: ExecOutcome::Live(LiveExec {
            client: spawn(cfg, req.events.clone()),
            // The tier the `IG_DEMO_*` trio authenticates, which `vike-mount` records the account
            // against: the arm has no live tier.
            bound_tier: Tier::Demo,
            grid: None,
            contract_size: None,
            margin_mode: None,
            leg_grids: Vec::new(),
        }),
        recon,
        identity: None,
    }
}

impl VenueMount for IgVenueMount {
    fn venue(&self) -> &'static str {
        VENUE
    }

    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration {
            addresses_accounts: true,
            process_exclusive: None,
            takes_recon_trigger: false,
            // An exec-only arm with NO grid pre-fetch: the mounted symbol keeps
            // `RiskLimits::new()`'s permissive `None`s, so a leg inherits nothing.
            grid_source: DeclaredGridSource::NoGrid,
            // `crates/bridges/ig/src/config.rs`'s `ig_env_var_names`: `IDENTIFIER` is the LOGIN.
            // ⚠ It names the login rather than the dealing account, and IG can hold several
            // accounts behind one login — so this row is sound in the direction that matters (two
            // labels with one identifier ARE one login, and this daemon selects that login's
            // default account both times) and silent in the other (two different logins on one
            // underlying account cannot be seen from here). A warning that fires only on a
            // certainty is the contract; the missed case is a miss, not a false report.
            book_identity: BookIdentity::Named {
                prefix: "IG",
                demo_tiers: &["DEMO"],
                live_tiers: &["LIVE", "MAINNET"],
                name_suffixes: &["IDENTIFIER"],
                evm_key_suffixes: &[],
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
        // Read BEFORE `req` moves into `mount_with`; the spawn closure hands it to the client.
        let halt_path = req.inputs.process.halt_path.clone();
        mount_with(
            req,
            move |cfg, events| {
                Box::new(
                    crate::exec::IgExecutionClient::spawn(cfg, events).with_halt_path(halt_path),
                )
            },
            crate::recon_client::recon_client,
        )
    }

    /// A labelled account is mounted only when it armed, so one with no demo trio never reaches
    /// `mount`; `vike-mount` asks for what `mount` would have said (see the trait method).
    fn report_unmounted_account(&self, inputs: &MountInputs<'_>) {
        Self::say_what_the_live_tier_is(inputs, Self::config(inputs).is_some());
    }

    /// `GET /session/encryptionKey` with the API key only — no login, no session. The tier mirrors
    /// the mount, which resolves `Environment::Demo` and nothing else; the DEFAULT account's key,
    /// as `vike-mount`'s `ig_time` read it. The parse (and its fixture test) lives beside the
    /// request, in `crates/bridges/ig/src/rest.rs`'s `parse_server_time_ms`.
    fn server_time_ms(&self, inputs: &MountInputs<'_>, timeout: Duration) -> Result<i64, String> {
        let Some(cfg) = load_ig_config_from(Environment::Demo, inputs.secrets) else {
            // The variable NAME comes from IG's own naming authority rather than a literal, so it
            // cannot rot against this crate.
            let (api_key_var, _, _) = ig_env_var_names(Environment::Demo);
            return Err(format!(
                "{api_key_var} is not set, so there is no key to read the clock with"
            ));
        };
        crate::rest::IgSession::server_time_ms(&cfg, timeout).map_err(|e| e.to_string())
    }
}

#[path = "mount_tests.rs"]
#[cfg(test)]
mod mount_tests;
