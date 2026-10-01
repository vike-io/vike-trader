//! fxcm's VENUE MOUNT — the `vike_bridge_core::venue_mount::VenueMount` contract over the
//! ForexConnect exec client and its dedicated reconcile session
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md).
//!
//! Moved, behaviour unchanged, from `vike-mount`: its `("fxcm", _)` arm (now
//! [`FxcmVenueMount::mount`]), its arming-probe row and the pure half it was built on (was
//! `fxcm_live_intent`, now this module's `resolution_for`), and its clock and book-identity rows
//! ([`FxcmVenueMount::declaration`], which also declares the `NoGrid` source this venue used to get
//! from `vike-mount`'s `legacy_grid_source` `_` arm — it never had a grid-source row of its own).
//! ONE deliberate text change: the clock row's reason, which said this venue had "no make_engine
//! arm" while it had one.
//!
//! ⚠ **THE LIVE GATE IS A PROPERTY OF THE BOX.** [`crate::sdk_available`] opens the ForexConnect
//! shim on THIS box once and reports what happened. Without it no `FxcmSession` can open, so an exec
//! client spawned there trades nothing: its session thread (`crate::exec`'s `run`) fails the login
//! and then REJECTS every command it is sent (`refuse_every_command`): each submit comes back as
//! `OrderSubmitted` and then a terminal `OrderRejected` naming `crate::SESSION_UNAVAILABLE` and the
//! loader's `FxcmError::Unavailable` text, so nothing vanishes — and nothing is placed. A mount
//! whose every order is refused is not a live mount, so this one REFUSES it out loud and lands on
//! paper (a simulated venue that fills), and `resolve` answers `SdkAbsent` before it looks at a
//! credential. That is what the gate buys: a box with credentials and no shim stays on paper
//! instead of mounting a client that reads LIVE and rejects everything. The refusal's log line used
//! to say such a client "would accept every order and discard it in silence" — true while a failed
//! login ended the session thread, false since `refuse_every_command` — and now says what the
//! client does. No CI runner stages the SDK, so every lane exercises the refusal; that the venue
//! TRADES is proven only on a box with the SDK staged, by hand, through
//! `crates/bridges/fxcm/tests/fxcm_live_smoke.rs`.
//!
//! ⚠ **BOTH `resolve` AND `mount` READ A PROCESS FACT BEYOND THEIR INPUTS: whether this box's shim
//! opened.** [`crate::sdk_available`] answers it from a `OnceLock` in `crate::loader`, which the
//! first caller in the process fills by `dlopen`ing the shim. Where the shim IS installed that first
//! call is not free — it opens with `RTLD_NOW`, which maps the ForexConnect library graph behind the
//! shim, and it asks the shim for its `fc_abi_version` — but it opens no session and makes no network
//! call, on either kind of box. For `resolve` this is the ONE exception the contract declares to its
//! PURE rule (`VenueMount::resolve`'s doc in `crates/vike-bridge-core/src/venue_mount.rs`); `mount`
//! reads the same cell, and its log lines read three more answers out of it —
//! [`crate::sdk_unavailable_reason`] on the refusal, [`crate::sdk_shim_path`] and
//! [`crate::sdk_abi_version`] on the LIVE line. It is a PROCESS fact rather than an input, so each
//! method is a one-line call into a private twin that takes the answer as a PARAMETER —
//! `resolution_for` for the decision, `mount_with` for the mount — and both answers are testable on
//! a box that has no SDK.
//!
//! ⚠ WEAKER ROBUSTNESS CONTRACT than cTrader's or IBKR's: `spawn` is INFALLIBLE, so there is no
//! connect result to demote on. A bad password or an unreachable gateway is discovered on the exec
//! session thread after the mount has returned, and a missing shim is the one failure this mount can
//! see beforehand — so it is the only one refused here. A live-but-failing login therefore mounts
//! "live" and trades nothing, and reconcile cannot notice: `crate::recon_client` logs in with the
//! same `FxcmConfig`, its login fails the same way, `FxcmReconClient::connect`'s balance probe
//! returns `None`, and nothing reconciles. What an operator sees after the mount's own LIVE line is
//! two `error!` lines — the exec session's and, with reconciliation on, the reconcile session's —
//! and every order rejected with the login's reason.
//!
//! Unlike the other feature-gated bridges this module is NOT behind this crate's `fxcm` feature,
//! which only lets `build.rs` try the shim compile: the module compiles the same code in every
//! build. What is feature-gated is the REGISTRATION — `vike-tradehub`'s `fxcm` feature owns this
//! crate's dependency, and a build without it registers fxcm `FeatureAbsent`.

use vike_bridge_core::credentials::Environment;
use vike_bridge_core::venue_mount::{
    BookIdentity, ClockDecl, DeclaredGridSource, ExecOutcome, HeldBelowLive, LiveExec, MountInputs,
    MountOutcome, MountRequest, PaperCause, Resolution, Tier, VenueDeclaration, VenueMount,
    recon_if_enabled,
};
use vike_exec::recon::ReconClient;

use crate::recon_client::VENUE;

/// fxcm's mount. `vike_tradehub::registry::REGISTRY` holds `&FxcmVenueMount` under that crate's `fxcm`
/// feature.
pub struct FxcmVenueMount;

/// The one credential gate the decision and the mount share: this account's DEMO login, on the
/// DEMO tier's `venue.fxcm.demo.{url,connection}` settings (`MountInputs::settings`, decision 0095).
fn config(inputs: &MountInputs<'_>) -> Option<crate::FxcmConfig> {
    crate::load_fxcm_config_for_account(
        Environment::Demo,
        inputs.account,
        inputs.secrets,
        inputs.settings,
    )
}

/// THE PURE DECISION, with the shim question as a PARAMETER so both answers are testable where
/// there is no SDK — which is every CI runner. The order is the legacy arming row's: the SHIM
/// first, then the credentials.
///
/// * **no shim** ⇒ `SdkAbsent`, whatever the store says: the mount refuses, so reporting a session
///   would raise a budget refusal over a venue that can only be paper (oanda's live-tier stance).
/// * **credentials** — the mount self-gates on the same DEMO loader, so an unconfigured store can
///   never raise a risk-budget refusal over a paper mount.
///
/// INTENT semantics on the remaining axis: a loadable shim with a WRONG password still resolves
/// armed — `spawn` is infallible and a failed login is only discovered after the mount returned, and
/// an operator who wrote credentials must carry a bounded budget either way. The account scopes the
/// credential half only; the shim is a property of the BOX, account-blind by construction.
fn resolution_for(sdk_available: bool, inputs: &MountInputs<'_>) -> Resolution {
    if !sdk_available {
        Resolution::Paper(PaperCause::SdkAbsent)
    } else if config(inputs).is_some() {
        Resolution::Armed { tier: Tier::Demo, held_below_live: Some(HeldBelowLive::DemoOnlyArm) }
    } else {
        Resolution::Paper(PaperCause::NoCredentials)
    }
}

/// THE MOUNT, with the two things it reaches beyond its request as PARAMETERS: whether this box's
/// shim opened, and the reconcile factory. [`FxcmVenueMount::mount`] passes
/// [`crate::sdk_available`] and `crate::recon_client`, and nothing else calls this in a build; it
/// exists so the LIVE branch — which no CI runner can reach through `mount` — and the ORDER of the
/// steps below are testable on any box.
fn mount_with(
    req: MountRequest<'_>,
    sdk_available: bool,
    recon_factory: impl FnOnce(&crate::FxcmConfig, &str) -> Option<Box<dyn ReconClient>>,
) -> MountOutcome {
    // The ARM's order, not the decision's: credentials FIRST — an unconfigured store is paper
    // with nothing said — then the shim, whose absence is refused out loud.
    let Some(cfg) = config(&req.inputs) else { return MountOutcome::paper() };
    if !sdk_available {
        // THE REFUSAL, carrying the loader's own diagnostic: "FXCM is not set up here" and
        // "FXCM is set up and the file is in the wrong place" are told apart only by the list
        // of paths that were tried.
        tracing::error!(
            venue = VENUE,
            reason = crate::sdk_unavailable_reason().unwrap_or("<none>"),
            "fxcm: credentials are present but the ForexConnect shim did not load \
             on this box, so a live mount could place nothing: its session could not log in \
             and every order and cancel would be REJECTED. REFUSING the live mount and \
             staying paper instead of mounting a live client that rejects everything. \
             `reason` lists every path that was tried (see \
             crates/bridges/fxcm/scripts/provision-fcsdk.sh and \
             docs/ops/fxcm-forexconnect.md)"
        );
        return MountOutcome::paper();
    }
    // Reconcile on its OWN dedicated ForexConnect session, so a reconcile table read never
    // contends the single-threaded, blocking exec session. Interval-only. LAZY: with
    // reconciliation off the second login never happens. ⚠ It is also the ONLY thing that
    // recovers fills a restart loses: the exec side routes an async fill through an in-process
    // map, so a re-surfaced trade after a restart is unroutable and dropped
    // (`crate::event_mapper`'s `map_drained_event` says so at `warn!`), and
    // `FxcmReconClient::fetch_fill_reports` reads the same Trades table by the same `trade_id`.
    let recon = recon_if_enabled(req.recon_enabled, || recon_factory(&cfg, req.symbol));
    tracing::warn!(
        connection = %cfg.connection,
        // WHICH RUNG answered, not merely that one did. The loader's ladder ends at the dynamic
        // loader's own search, whose rung is the bare file name, so on a box that took it this
        // reads `libfcshim.so` and names no file (`ldd` on the running binary resolves it).
        shim = crate::sdk_shim_path().unwrap_or("<unknown>"),
        // WHAT THAT SHIM CAN DO: `0` means it predates `fc_login_ex`, so a login failure can say
        // THAT it failed and never why. Never a reason to refuse — an old shim executes orders
        // exactly as it always did.
        shim_abi = crate::sdk_abi_version().unwrap_or(0),
        "fxcm: config present and the ForexConnect shim loaded → LIVE exec \
         client (real orders on the resolved account)"
    );
    MountOutcome {
        exec: ExecOutcome::Live(LiveExec {
            client: Box::new(
                crate::FxcmExecutionClient::spawn(cfg, req.events.clone())
                    .with_halt_path(req.inputs.process.halt_path.clone()),
            ),
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

impl VenueMount for FxcmVenueMount {
    fn venue(&self) -> &'static str {
        VENUE
    }

    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration {
            addresses_accounts: true,
            process_exclusive: None,
            // Interval-only reconcile: `vike_mount::build_node` threads no trigger for this venue.
            takes_recon_trigger: false,
            // No symbol-properties endpoint short of a session-bound SDK call, so the mounted
            // symbol keeps `RiskLimits::new()`'s permissive `None`s and a leg inherits nothing.
            grid_source: DeclaredGridSource::NoGrid,
            // ⚠ **CORRECTED** — this row was once `Named { name_suffixes: &["USER"] }`, on the claim
            // that the login IS the account. `crates/bridges/fxcm/src/shim/fcshim.cpp`'s
            // `firstAccount` reads an ACCOUNTS TABLE off the login rules and returns the first row
            // that is kind 32/36 and whose margin-call flag is `N`, and `fc_place`, `fc_account` and
            // `fc_base_unit_size` each call it afresh — so one login reaches several accounts, `USER`
            // names the login rather than the book, and a login with two eligible accounts moves its
            // flow to the second when the first enters a margin call (an ORDER-ROUTING defect,
            // recorded in `crates/bridges/fxcm/CLAUDE.md`). `Undeterminable` is the safe direction:
            // two accounts of one login would otherwise compare equal and be reported as a shared
            // book they may not share.
            book_identity: BookIdentity::Undeterminable {
                why: "the store holds a ForexConnect LOGIN (`FXCM_{TIER}_USER`), and one login reaches \
                      an ACCOUNTS TABLE rather than one account — the shim picks the first row that is \
                      kind 32/36 and not in margin call, afresh on every FFI call. So nothing in the \
                      store names the book, and the book is not fixed for the session either",
            },
            // The clock leg lists fxcm only when the venue would mount live — the shim loaded and
            // a login present — and even then there is no REST clock to read. ⚠ This reason is the
            // port's one deliberate text change: it said "no make_engine arm", false while the arm
            // existed (the venue mount contract spec's Finding 3).
            clock: ClockDecl::NotWired {
                reason: "no REST surface at all: the venue's only clock lives inside the native \
                         ForexConnect FFI session, which this pre-mount step does not open",
                unmeasured_risk: None,
            },
        }
    }

    /// The pure decision at THIS process's shim. [`crate::sdk_available`] opens the shim once per
    /// process and caches the answer — the one read `resolve` makes beyond the credential map, and
    /// the same one the legacy arming row made (this module's doc carries why that read is allowed).
    fn resolve(&self, inputs: &MountInputs<'_>) -> Resolution {
        resolution_for(crate::sdk_available(), inputs)
    }

    /// `mount_with` at THIS process's shim, with the real reconcile factory.
    fn mount(&self, req: MountRequest<'_>) -> MountOutcome {
        mount_with(req, crate::sdk_available(), crate::recon_client)
    }
}

#[path = "mount_tests.rs"]
#[cfg(test)]
mod mount_tests;
