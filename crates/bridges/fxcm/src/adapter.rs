//! fxcm's VENUE ADAPTER — the `vike_bridge_core::venue_adapter::VenueAdapter` contract over the
//! ForexConnect exec client and its dedicated reconcile session
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md).
//!
//! Moved, behaviour unchanged, from `vike-tradehub-core`: its `("fxcm", _)` arm (now
//! [`FxcmVenueAdapter::start`]), its arming-probe row and the pure half it was built on (was
//! `fxcm_live_intent`, now this module's `resolution_for`), and its clock and book-identity rows
//! ([`FxcmVenueAdapter::declaration`], which also declares the `NoGrid` source this venue used to get
//! from `vike-tradehub-core`'s `legacy_grid_source` `_` arm — it never had a grid-source row of its own).
//! ONE deliberate text change: the clock row's reason, which said this venue had "no make_engine
//! arm" while it had one.
//!
//! ⚠ **THE LIVE GATE IS A PROPERTY OF THE BOX.** [`crate::sdk_available`] opens the ForexConnect
//! shim on THIS box once and reports what happened. Without it no `FxcmSession` can open, so an exec
//! client spawned there trades nothing: its session thread (`crate::exec`'s `run`) fails the login
//! and then REJECTS every command it is sent (`refuse_every_command`): each submit comes back as
//! `OrderSubmitted` and then a terminal `OrderRejected` naming `crate::SESSION_UNAVAILABLE` and the
//! loader's `FxcmError::Unavailable` text, so nothing vanishes — and nothing is placed. A start
//! whose every order is refused is not a live start, so this one REFUSES it out loud and lands on
//! paper (a simulated venue that fills), and `resolve` answers `SdkAbsent` before it looks at a
//! credential. That is what the gate buys: a box with credentials and no shim stays on paper
//! instead of starting a client that reads LIVE and rejects everything. The refusal's log line used
//! to say such a client "would accept every order and discard it in silence" — true while a failed
//! login ended the session thread, false since `refuse_every_command` — and now says what the
//! client does. No CI runner stages the SDK, so every lane exercises the refusal; that the venue
//! TRADES is proven only on a box with the SDK staged, by hand, through
//! `crates/bridges/fxcm/tests/fxcm_live_smoke.rs`.
//!
//! ⚠ **BOTH `resolve` AND `start` READ A PROCESS FACT BEYOND THEIR INPUTS: whether this box's shim
//! opened.** [`crate::sdk_available`] answers it from a `OnceLock` in `crate::loader`, which the
//! first caller in the process fills by `dlopen`ing the shim. Where the shim IS installed that first
//! call is not free — it opens with `RTLD_NOW`, which maps the ForexConnect library graph behind the
//! shim, and it asks the shim for its `fc_abi_version` — but it opens no session and makes no network
//! call, on either kind of box. For `resolve` this is the ONE exception the contract declares to its
//! PURE rule (`VenueAdapter::resolve`'s doc in `crates/vike-bridge-core/src/venue_adapter.rs`); `start`
//! reads the same cell, and its log lines read three more answers out of it —
//! [`crate::sdk_unavailable_reason`] on the refusal, [`crate::sdk_shim_path`] and
//! [`crate::sdk_abi_version`] on the LIVE line. It is a PROCESS fact rather than an input, so each
//! method is a one-line call into a private twin that takes the answer as a PARAMETER —
//! `resolution_for` for the decision, `start_with` for the start — and both answers are testable on
//! a box that has no SDK.
//!
//! ⚠ WEAKER ROBUSTNESS CONTRACT than cTrader's or IBKR's: `spawn` is INFALLIBLE, so there is no
//! connect result to demote on. A bad password or an unreachable gateway is discovered on the exec
//! session thread after the start has returned, and a missing shim is the one failure this start can
//! see beforehand — so it is the only one refused here. A live-but-failing login therefore starts
//! "live" and trades nothing, and reconcile cannot notice: `crate::recon_client` logs in with the
//! same `FxcmConfig`, its login fails the same way, `FxcmReconClient::connect`'s balance probe
//! returns `None`, and nothing reconciles. What an operator sees after the start's own LIVE line is
//! two `error!` lines — the exec session's and, with reconciliation on, the reconcile session's —
//! and every order rejected with the login's reason.
//!
//! Unlike the other feature-gated bridges this module is NOT behind this crate's `fxcm` feature,
//! which only lets `build.rs` try the shim compile: the module compiles the same code in every
//! build. What is feature-gated is the REGISTRATION — `vike-tradehub`'s `fxcm` feature owns this
//! crate's dependency, and a build without it registers fxcm `FeatureAbsent`.
//!
//! ⚠ **A LIVE ACCOUNT'S LOGIN IS NAMED, NOT IGNORED.** This arm starts only the DEMO login. An
//! account has ONE tier and its login is that tier's (decision 0156 item 6), so a `live` account
//! holding a complete login holds one this arm will not use: it answers
//! [`PaperCause::LiveTierNotWired`] rather than `NoCredentials` (the words of an empty account),
//! and `start` says so at `error!` (`vike_bridge_core::venue_adapter::report_live_tier_not_wired`).
//! No venue starts live that did not before. The same function speaks for a labelled account that
//! is never started ([`VenueAdapter::report_not_running_account`]); an incomplete or empty account is
//! silent.
//!
//! ⚠ **ONE ORDER, TWO SURFACES.** `resolve` (the arming screen, `vike-backend venues`) used to ask the
//! shim first and `start` (the log) the credentials first, so a box without the ForexConnect shim
//! holding only a LIVE login printed `SdkAbsent` on the screen and `LiveTierNotWired` in the log.
//! Both were true, and an operator reading one then the other had two answers to one question. They
//! now ask in ONE order — `resolution_for` is the rule and `start_with` follows it:
//!
//! 1. **a LIVE account's login** is named first (`LiveTierNotWired`), on every box — it is a fact
//!    about the ACCOUNT, which no box can change, and the shim cannot make that login any more
//!    usable;
//! 2. **the shim**: without it nothing arms (`SdkAbsent`), whatever else the store holds — a fact
//!    about the BOX;
//! 3. **the demo login**: armed with the shim, `NoCredentials` without one.
//!
//! The log speaks only of a credential that is PRESENT and unusable (the doctrine: an absent one is
//! silent), so an unconfigured store is silent at `start` while the screen still says the shim is
//! missing — the screen is where a missing shim is told. Nothing here changes what starts: every cell
//! that was paper is paper, and the one that armed is the one that armed
//! (`crates/vike-gatewayhub/tests/trade_roster/fxcm_adapter.rs` holds the whole table, and the cells that changed).

use vike_bridge_core::venue_adapter::{
    AdapterInputs, AdapterOutcome, AdapterRequest, BookIdentity, ClockDecl, DeclaredGridSource,
    ExecOutcome, HeldBelowLive, LiveExec, PaperCause, Resolution, VenueAdapter, VenueDeclaration,
    recon_if_enabled, report_live_tier_not_wired,
};
use vike_exec::recon::ReconClient;
use vike_model::AccountTier;

use crate::config::{DemoLogin, demo_login};
use crate::recon_client::VENUE;

/// fxcm's start. `vike_tradehub::registry::REGISTRY` holds `&FxcmVenueAdapter` under that crate's `fxcm`
/// feature.
pub struct FxcmVenueAdapter;

/// The one credential answer the decision and the start share: this account's login at this
/// account's tier, on the DEMO tier's `venue.fxcm.demo.{url,connection}` settings
/// (`AdapterInputs::settings`, decision 0095) — `crate::config`'s `demo_login`.
fn login(inputs: &AdapterInputs<'_>) -> DemoLogin {
    demo_login(inputs.tier, &inputs.credentials, inputs.settings)
}

/// **Everything this arm says about the LIVE tier**, in one place: `start` calls it for the account
/// it is starting and [`VenueAdapter::report_not_running_account`] for a labelled account that is never
/// started, so the two cannot word one fact two ways. Only a `live` account holding its whole login
/// is said — whatever the shim says, since it is a fact about the account; anything else is silent.
fn say_what_the_live_tier_is(inputs: &AdapterInputs<'_>, login: &DemoLogin) {
    if matches!(login, DemoLogin::LiveTierNotWired) {
        report_live_tier_not_wired(VENUE, inputs.account, "demo");
    }
}

/// THE PURE DECISION, with the shim question as a PARAMETER so both answers are testable where
/// there is no SDK — which is every CI runner. **This is the ONE ORDER the arming screen and the log
/// both follow** (see the module doc's "ONE ORDER, TWO SURFACES"): a LIVE account's login, then the
/// shim, then the demo login.
///
/// * **a LIVE account's complete login** answers [`PaperCause::LiveTierNotWired`] FIRST, on a box
///   with the shim and on one without. It is a fact about the ACCOUNT that no shim can change, and
///   it is the cause `start` reports at `error!` — so the screen and the log say the same thing. It
///   used to wait behind the shim question, which put `SdkAbsent` on the screen beside an `error!`
///   saying the login would never be used.
/// * **no shim** ⇒ `SdkAbsent`, whatever else the store says: the start refuses, so reporting a
///   session would raise a budget refusal over a venue that can only be paper (oanda's live-tier
///   stance).
/// * **the demo login** — the start self-gates on the same DEMO loader, so an unconfigured store can
///   never raise a risk-budget refusal over a paper start.
///
/// INTENT semantics on the remaining axis: a loadable shim with a WRONG password still resolves
/// armed — `spawn` is infallible and a failed login is only discovered after the start returned, and
/// an operator who wrote credentials must carry a bounded budget either way. The account scopes the
/// credential half only; the shim is a property of the BOX, account-blind by construction.
fn resolution_for(sdk_available: bool, inputs: &AdapterInputs<'_>) -> Resolution {
    match login(inputs) {
        DemoLogin::LiveTierNotWired => Resolution::Paper(PaperCause::LiveTierNotWired),
        _ if !sdk_available => Resolution::Paper(PaperCause::SdkAbsent),
        DemoLogin::Demo(_) => Resolution::Armed {
            tier: AccountTier::Demo,
            held_below_live: Some(HeldBelowLive::DemoOnlyArm),
        },
        DemoLogin::Unconfigured => Resolution::Paper(PaperCause::NoCredentials),
    }
}

/// THE START, with the two things it reaches beyond its request as PARAMETERS: whether this box's
/// shim opened, and the reconcile factory. [`FxcmVenueAdapter::start`] passes
/// [`crate::sdk_available`] and `crate::recon_client`, and nothing else calls this in a build; it
/// exists so the LIVE branch — which no CI runner can reach through `start` — and the ORDER of the
/// steps below are testable on any box.
fn start_with(
    req: AdapterRequest<'_>,
    sdk_available: bool,
    recon_factory: impl FnOnce(&crate::FxcmConfig, &str) -> Option<Box<dyn ReconClient>>,
) -> AdapterOutcome {
    // The decision's order ([`resolution_for`]): the LIVE account first — an unconfigured account
    // is paper with nothing said, but a live account's complete login is said out loud — then the
    // shim, whose absence is refused out loud. An unconfigured account never reaches the shim
    // question here: the log speaks only of a credential that is present, and the screen is where a
    // missing shim is told.
    let held = login(&req.inputs);
    say_what_the_live_tier_is(&req.inputs, &held);
    let DemoLogin::Demo(cfg) = held else {
        return AdapterOutcome::paper();
    };
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
        return AdapterOutcome::paper();
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
        venue = VENUE,
        account = %req.inputs.account,
        tier = AccountTier::Demo.as_str(),
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
    AdapterOutcome {
        exec: ExecOutcome::Live(LiveExec {
            client: Box::new(
                crate::FxcmExecutionClient::spawn(cfg, req.events.clone())
                    .with_halt_path(req.inputs.process.halt_path.clone()),
            ),
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

impl VenueAdapter for FxcmVenueAdapter {
    fn venue(&self) -> &'static str {
        VENUE
    }

    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration {
            addresses_accounts: true,
            process_exclusive: None,
            // Interval-only reconcile: `vike_tradehub_core::assemble_runtime` threads no trigger for this venue.
            takes_recon_trigger: false,
            // No symbol-properties endpoint short of a session-bound SDK call, so the started
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
                why: "the store holds a ForexConnect LOGIN (the account's `USER` field), and one \
                      login reaches an ACCOUNTS TABLE rather than one account — the shim picks the \
                      first row that is kind 32/36 and not in margin call, afresh on every FFI \
                      call. So nothing in the store names the book, and the book is not fixed for \
                      the session either",
            },
            // The clock leg lists fxcm only when the venue would start live — the shim loaded and
            // a login present — and even then there is no REST clock to read. ⚠ This reason is the
            // port's one deliberate text change: it said "no make_engine arm", false while the arm
            // existed (the venue-adapter contract spec's Finding 3).
            clock: ClockDecl::NotWired {
                reason: "no REST surface at all: the venue's only clock lives inside the native \
                         ForexConnect FFI session, which this pre-mount step does not open",
                unmeasured_risk: None,
            },
        }
    }

    /// The pure decision at THIS process's shim. [`crate::sdk_available`] opens the shim once per
    /// process and caches the answer — the one read `resolve` makes beyond its inputs, and
    /// the same one the legacy arming row made (this module's doc carries why that read is allowed).
    fn resolve(&self, inputs: &AdapterInputs<'_>) -> Resolution {
        resolution_for(crate::sdk_available(), inputs)
    }

    /// `start_with` at THIS process's shim, with the real reconcile factory.
    fn start(&self, req: AdapterRequest<'_>) -> AdapterOutcome {
        // The reconcile session registers under the ACCOUNT's label (`""` = the default account), which is
        // how the gatewayhub's facts lane finds it again (`crate::reads`).
        let account = req.inputs.account.text().unwrap_or_default().to_string();
        start_with(req, crate::sdk_available(), move |cfg, symbol| {
            crate::recon_client_for(cfg, symbol, &account)
        })
    }

    /// A labelled account is started only when it armed, so a `live` one — or one held back by the
    /// missing shim — never reaches `start`; `vike-tradehub-core` asks for what `start` would have said
    /// (see the trait method). The shim is not consulted: what is said is about the ACCOUNT, so a
    /// labelled live account on a box without the shim hears about its login exactly as the default
    /// account does.
    fn report_not_running_account(&self, inputs: &AdapterInputs<'_>) {
        say_what_the_live_tier_is(inputs, &login(inputs));
    }
}

#[path = "adapter_tests.rs"]
#[cfg(test)]
mod adapter_tests;
