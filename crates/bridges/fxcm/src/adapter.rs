//! fxcm's VENUE ADAPTER — the `vike_bridge_core::venue_adapter::VenueAdapter` contract over the
//! ForexConnect exec client and its dedicated reconcile session
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md).
//!
//! Moved, behaviour unchanged, from `vike-tradehub-core`: its `("fxcm", _)` arm (now
//! [`VenueAdapterFxcm::start`]), its arming-probe row and the pure half it was built on (was
//! `fxcm_live_intent`, now this module's `resolution_for`), and its clock and book-identity rows
//! ([`VenueAdapterFxcm::declaration`], which also declares the `NoGrid` source this venue used to get
//! from `vike-tradehub-core`'s `legacy_grid_source` `_` arm — it never had a grid-source row of its own).
//! ONE deliberate text change: the clock row's reason, which said this venue had "no make_engine
//! arm" while it had one.
//!
//! ⚠ **THE LIVE GATE IS A PROPERTY OF THE BOX.** [`crate::sdk_available`] opens the ForexConnect
//! shim on THIS box once and reports what happened. Without it no `FxcmSession` can open, so an exec
//! client spawned there trades nothing: its session thread (`crate::execution`'s `run`) fails the login
//! and then REFUSES every command it is sent (`refuse_every_command`): each submit comes back as
//! `OrderSubmitted` and then a terminal `OrderRejected` naming `crate::SESSION_UNAVAILABLE` and the
//! loader's `FxcmError::Unavailable` text, so nothing vanishes — and nothing is placed. A start
//! whose every order is refused is not a live start, so this one REFUSES it out loud and the account
//! does NOT RUN (decision 0156), and `resolve` answers `SdkAbsent` before it looks at a
//! credential. That is what the gate buys: on a box with credentials and no shim the account says it
//! is not running, instead of starting a client that reads LIVE and rejects everything. The refusal's log line used
//! to say such a client "would accept every order and discard it in silence" — true while a failed
//! login ended the session thread, false since `refuse_every_command` — and now says what the
//! client does. No CI runner stages the SDK, so every lane exercises the refusal; that the venue
//! TRADES is proven only on a box with the SDK staged, by hand, through
//! `crates/bridges/fxcm/tests/fxcm_smokes/fxcm_live_smoke.rs`.
//!
//! ⚠ **BOTH `resolve` AND `start` READ A PROCESS FACT BEYOND THEIR INPUTS: whether this box's shim
//! opened.** [`crate::sdk_available`] answers it from a `OnceLock` in `crate::transport::loader`, which the
//! first caller in the process fills by `dlopen`ing the shim. Where the shim IS installed that first
//! call is not free — it opens with `RTLD_NOW`, which maps the ForexConnect library graph behind the
//! shim, and it asks the shim for its `forex_connect_abi_version` — but it opens no session and makes no network
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
//! see beforehand — so it is refused here, and so is a CFD mount whose Offers row (what one unit of it
//! stands for) cannot be read over the reconcile session (`mount_value`); a currency pair reads nothing. That CFD
//! refusal follows its cause's kind (`AdapterOutcome::failed`): a reconcile login that never reached FXCM
//! (`crate::transport::sys::FxcmError::kind`: a host discovery with no HTTP answer, no answer at all) or an Offers
//! read nobody answered in time ends the start `Unreachable` and the tradehub asks it again, anything else waits for
//! the operator. A live-but-failing login therefore starts
//! "live" and trades nothing, and reconcile cannot notice at the start: `crate::reconcile` logs
//! in with the same `FxcmConfig` and its login fails the same way. What an operator sees after the
//! start's own LIVE line is two `error!` lines — the exec session's and, with reconciliation on, the
//! reconcile session's — and every order rejected with the login's reason.
//!
//! ⚠ **BOTH SESSIONS ARE THEN TRIED AGAIN, for the life of the mount** (`crate::reconcile::redial`): after 30 s,
//! 1 min, 2 min, 5 min and then every 10 min, one `warn!` line per failed attempt and one `info!` line
//! when a session opens. A host that was down for a weekend close therefore no longer kills the
//! account until a process restart. A login the venue REFUSED over its credentials is the exception:
//! it is never retried (`crate::reconcile::redial::refuses_credentials`), because a rotated password does not heal
//! and every retry is another failed login. The exec thread rejects every command, with the
//! `crate::SESSION_UNAVAILABLE` marker, until its retry opens; the reconcile client
//! (`crate::reconcile_client_redialing_for`) answers every read with an error until its own does. The
//! account's currency, though, was told at the START: a start that found no reconcile session told
//! an ASSUMED USD, and the node anchors the engine to it until the account is started again.
//!
//! Unlike the other feature-gated bridges this module is NOT behind this crate's `fxcm` feature,
//! which only lets `build.rs` try the shim compile: the module compiles the same code in every
//! build. What is feature-gated is the REGISTRATION — `vike-tradehub`'s `fxcm` feature owns this
//! crate's dependency, and a build without it registers fxcm `FeatureAbsent`.
//!
//! ⚠ **A LIVE ACCOUNT'S LOGIN IS NAMED, NOT IGNORED.** This arm starts only the DEMO login. An
//! account has ONE tier and its login is that tier's (decision 0156 item 6), so a `live` account
//! holding a complete login holds one this arm will not use: it answers
//! [`ArmingNotRunningCause::LiveTierNotWired`] rather than `NoCredentials` (the words of an empty account),
//! and `start` says so at `error!` (`vike_bridge_core::venue_adapter::ArmingDemoOnly::report_live_tier`).
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

use vike_bridge_core::transport::ErrorKind;
use vike_bridge_core::venue_adapter::{
    AdapterInputs, AdapterOutcome, AdapterRequest, ArmingDemoOnly, ArmingNotRunningCause,
    ArmingResolution, BookIdentityRule, ClockDeclaration, VenueAdapter, VenueAdapterDeclaration,
    VenueAdapterExecutionOutcome, VenueAdapterExecutionRunning, reconcile_if_enabled,
    report_address_refused,
};
use vike_bridge_core::venue_link::venue_link;
use vike_execution::recon::ReconcileClient;
use vike_model::{AccountTier, ContractValue, GridSource};
use vike_venues::venue_urls::UrlRefusal;

use crate::config::demo_login;
use crate::reconcile::VENUE;

/// fxcm's start. `vike_tradehub::registry::REGISTRY` holds `&VenueAdapterFxcm` under that crate's `fxcm`
/// feature.
pub struct VenueAdapterFxcm;

/// The one credential answer the decision and the start share: this account's login at this
/// account's tier, on the `fxcm demo login rest Demo` address row (`AdapterInputs::settings`'s
/// `urls`) — `crate::config`'s `demo_login`. Everything the arm says about the LIVE tier is its
/// `report_live_tier`: `start` calls it for the account it is starting and
/// [`VenueAdapter::report_not_running_account`] for a labelled account that is never started, so the
/// two cannot word one fact two ways. Only a `live` account holding its whole login is said —
/// whatever the shim says, since it is a fact about the account; anything else is silent.
fn login(inputs: &AdapterInputs<'_>) -> ArmingDemoOnly<Result<crate::FxcmConfig, UrlRefusal>> {
    demo_login(inputs.tier, &inputs.credentials, inputs.settings)
}

/// THE PURE DECISION, with the shim question as a PARAMETER so both answers are testable where
/// there is no SDK — which is every CI runner. **This is the ONE ORDER the arming screen and the log
/// both follow** (see the module doc's "ONE ORDER, TWO SURFACES"): a LIVE account's login, then the
/// shim, then the demo login.
///
/// * **a LIVE account's complete login** answers [`ArmingNotRunningCause::LiveTierNotWired`] FIRST, on a box
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
fn resolution_for(sdk_available: bool, inputs: &AdapterInputs<'_>) -> ArmingResolution {
    match login(inputs) {
        ArmingDemoOnly::LiveTierNotWired => {
            ArmingResolution::NotRunning(ArmingNotRunningCause::LiveTierNotWired)
        }
        _ if !sdk_available => ArmingResolution::NotRunning(ArmingNotRunningCause::SdkAbsent),
        // DEMO held below live, or `NoCredentials`.
        arm => arm.resolution(),
    }
}

/// **What one unit of the mounted `symbol` stands for**: nothing to read for a currency pair (1, its class's rule:
/// the catalog's name answer, `vike_catalog::contract_value`, which `account/quote_rate_tests.rs`'s
/// `the_catalogs_value_of_a_name_is_the_rows_or_none` holds to every committed Offers row), else its own row's
/// multiplier (`crate::account::quote_rate::mount_contract_value`) read over the account's RECONCILE session
/// (`crate::account::reads`; a second login is not opened, FXCM sessions are scarce).
///
/// # Errors
/// A CFD with no reconcile session up for `account`, an Offers table that cannot be read, or no usable row, each
/// with its kind. No session because its first login failed is THAT login's kind (`first_login`,
/// `crate::transport::sys::FxcmError::kind`: a host it never reached is asked again by itself); an Offers read that
/// failed is the READ's kind (`crate::account::reads::SessionReads::offers_with_kind`: a session or a refresh that
/// did not answer in time is asked again by itself); reconciliation off, a read the venue failed (its words do not
/// tell a transient failure from a refusal) and no usable row are `ErrorKind::Unknown`, which waits for the operator.
fn mount_value(
    symbol: &str,
    account: &str,
    first_login: Option<ErrorKind>,
) -> Result<Option<ContractValue>, (ErrorKind, String)> {
    if vike_catalog::contract_value(VENUE, symbol).is_some() {
        return Ok(None);
    }
    let reads = crate::account::reads::account_reads(account).ok_or_else(|| {
        (
            first_login.unwrap_or(ErrorKind::Unknown),
            "no reconcile session is up for this account (reconciliation is off, or its first login \
             failed), and a CFD's multiplier is read over it"
                .to_string(),
        )
    })?;
    crate::account::quote_rate::mount_contract_value(&reads.offers_with_kind()?, symbol)
        .map_err(|why| (ErrorKind::Unknown, why.to_string()))
}

/// THE START, with the two things it reaches beyond its request as PARAMETERS: whether this box's
/// shim opened, and the reconcile factory. [`VenueAdapterFxcm::start`] passes
/// [`crate::sdk_available`] and `crate::reconcile`, and nothing else calls this in a build; it
/// exists so the LIVE branch — which no CI runner can reach through `start` — and the ORDER of the
/// steps below are testable on any box.
///
/// `reconcile_factory` answers the client to keep and its first login's failure kind (`None` when it opened):
/// `crate::reconcile_client_redialing_for`'s pair.
fn start_with(
    req: AdapterRequest<'_>,
    sdk_available: bool,
    reconcile_factory: impl FnOnce(
        &crate::FxcmConfig,
        &str,
    ) -> (Option<Box<dyn ReconcileClient>>, Option<ErrorKind>),
) -> AdapterOutcome {
    // The decision's order ([`resolution_for`]): the LIVE account first — an unconfigured account
    // is paper with nothing said, but a live account's complete login is said out loud — then the
    // shim, whose absence is refused out loud. An unconfigured account never reaches the shim
    // question here: the log speaks only of a credential that is present, and the screen is where a
    // missing shim is told.
    let held = login(&req.inputs);
    held.report_live_tier(VENUE, req.inputs.account);
    let ArmingDemoOnly::Demo(config) = held else {
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
             and every order and cancel would be REJECTED. REFUSING the live mount: this account \
             does NOT RUN, instead of mounting a live client that rejects everything. \
             `reason` lists every path that was tried (see \
             crates/bridges/fxcm/scripts/provision-fcsdk.sh and \
             docs/ops/fxcm-forexconnect.md)"
        );
        return AdapterOutcome::paper();
    }
    // A whole login and no Hosts.jsp row: refused by name, nothing dialled. Both sessions' login retries wait on the
    // FX login gate's clock the start was handed.
    let config = match config {
        Ok(config) => {
            crate::config::FxcmConfig { fx_clock: req.inputs.fx_clock.cloned(), ..config }
        }
        Err(refusal) => {
            report_address_refused(VENUE, req.inputs.account, refusal);
            return AdapterOutcome::paper();
        }
    };
    // Reconcile on its OWN dedicated ForexConnect session, so a reconcile table read never
    // contends the single-threaded, blocking exec session. Interval-only. LAZY: with
    // reconciliation off the second login never happens. ⚠ It is also the ONLY thing that
    // recovers fills a restart loses: the exec side routes an async fill through an in-process
    // map, so a re-surfaced trade after a restart is unroutable and dropped
    // (`crate::execution::event_mapper`'s `map_drained_event` says so at `warn!`), and
    // `ReconcileClientFxcm::fetch_fill_reports` reads the same Trades table by the same `trade_id`.
    let mut first_login = None;
    let recon = reconcile_if_enabled(req.reconcile_enabled, || {
        let (client, failed) = reconcile_factory(&config, req.symbol);
        first_login = failed;
        client
    });
    // ⚠ NO ROW, NO VALUE, NO MOUNT, for a CFD. One unit of a metal, index or commodity is its row's
    // `ContractMultiplier` units of the underlying (Copper 100, US30 0.1), the figure an admitted symbol is already
    // booked by; booked at 1 the engine would size every notional, margin and PnL by the wrong factor. A currency
    // pair needs no read. A CFD whose row cannot be read does not run (the reconcile client above is dropped with
    // the outcome, and unregisters its session): asked again by itself when the reconcile login never reached
    // FXCM or the Offers read was not answered in time, else until the operator acts.
    let account = req.inputs.account.text().unwrap_or_default();
    let contract_value = match mount_value(req.symbol, account, first_login) {
        Ok(value) => value,
        Err((kind, why)) => {
            tracing::error!(
                venue = VENUE,
                account = %req.inputs.account,
                symbol = req.symbol,
                %kind,
                "fxcm: REFUSED — what one unit of {} stands for could not be read ({why}), and a guess \
                 would size every order at the wrong notional; the account is NOT RUNNING {}",
                req.symbol,
                if kind.is_terminal() {
                    "until its Offers row can be read (this failure waits for the operator)"
                } else {
                    "yet: its start is asked again by itself until the Offers row can be read"
                }
            );
            return AdapterOutcome::failed(kind);
        }
    };
    tracing::warn!(
        venue = VENUE,
        account = %req.inputs.account,
        tier = AccountTier::Demo.as_str(),
        connection = %config.connection,
        // WHICH RUNG answered, not merely that one did. The loader's ladder ends at the dynamic
        // loader's own search, whose rung is the bare file name, so on a box that took it this
        // reads `libfcshim.so` and names no file (`ldd` on the running binary resolves it).
        shim = crate::sdk_shim_path().unwrap_or("<unknown>"),
        // WHAT THAT SHIM CAN DO: `0` means it predates `forex_connect_login_ex`, so a login failure can say
        // THAT it failed and never why. Never a reason to refuse — an old shim executes orders
        // exactly as it always did.
        shim_abi = crate::sdk_abi_version().unwrap_or(0),
        "fxcm: config present and the ForexConnect shim loaded → LIVE exec \
         client (real orders on the resolved account)"
    );
    AdapterOutcome {
        exec: VenueAdapterExecutionOutcome::Running(VenueAdapterExecutionRunning {
            client: Box::new(
                crate::execution::spawn_execution_client_reporting(
                    config,
                    req.events.clone(),
                    // The account's one link cell, by the route key its events carry (the gatewayhub reads it there).
                    req.events.route_key.map(|key| venue_link(&key)).unwrap_or_default(),
                )
                .with_halt_path(req.inputs.process.halt_path.clone()),
            ),
            bound_tier: AccountTier::Demo,
            grid: None,
            contract_value,
            margin_mode: None,
            leg_grids: Vec::new(),
        }),
        reconcile: recon,
        identity: None,
    }
}

impl VenueAdapter for VenueAdapterFxcm {
    fn venue(&self) -> &'static str {
        VENUE
    }

    fn declaration(&self) -> VenueAdapterDeclaration {
        VenueAdapterDeclaration {
            addresses_accounts: true,
            process_exclusive: None,
            // Interval-only reconcile: `vike_tradehub_core::assemble_runtime` threads no trigger for this venue.
            // No symbol-properties endpoint short of a session-bound SDK call, so the started
            // symbol keeps `RiskLimits::new()`'s permissive `None`s and a leg inherits nothing.
            grid_source: GridSource::NoGrid,
            // ⚠ **CORRECTED** — this row was once `Named { name_suffixes: &["USER"] }`, on the claim
            // that the login IS the account. `crates/bridges/fxcm/src/shim/fcshim.cpp`'s
            // `firstAccount` reads an ACCOUNTS TABLE off the login rules and returns the first row
            // that is kind 32/36 and whose margin-call flag is `N`, and `forex_connect_place`, `forex_connect_account` and
            // `forex_connect_base_unit_size` each call it afresh — so one login reaches several accounts, `USER`
            // names the login rather than the book, and a login with two eligible accounts moves its
            // flow to the second when the first enters a margin call (an ORDER-ROUTING defect,
            // recorded in `crates/bridges/fxcm/CLAUDE.md`). `Undeterminable` is the safe direction:
            // two accounts of one login would otherwise compare equal and be reported as a shared
            // book they may not share.
            book_identity: BookIdentityRule::Undeterminable {
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
            clock: ClockDeclaration::NotWired {
                reason: "no REST surface at all: the venue's only clock lives inside the native \
                         ForexConnect FFI session, which this pre-mount step does not open",
                unmeasured_risk: None,
            },
        }
    }

    /// The pure decision at THIS process's shim. [`crate::sdk_available`] opens the shim once per
    /// process and caches the answer — the one read `resolve` makes beyond its inputs, and
    /// the same one the legacy arming row made (this module's doc carries why that read is allowed).
    fn resolve(&self, inputs: &AdapterInputs<'_>) -> ArmingResolution {
        resolution_for(crate::sdk_available(), inputs)
    }

    /// `start_with` at THIS process's shim, with the real reconcile factory.
    fn start(&self, req: AdapterRequest<'_>) -> AdapterOutcome {
        // The reconcile session registers under the ACCOUNT's label (`""` = the default account), which is
        // how the gatewayhub's facts lane finds it again (`crate::account::reads`).
        let account = req.inputs.account.text().unwrap_or_default().to_string();
        start_with(req, crate::sdk_available(), move |config, symbol| {
            crate::reconcile_client_redialing_for(config, symbol, &account)
        })
    }

    /// A labelled account is started only when it armed, so a `live` one — or one held back by the
    /// missing shim — never reaches `start`; `vike-tradehub-core` asks for what `start` would have said
    /// (see the trait method). The shim is not consulted: what is said is about the ACCOUNT, so a
    /// labelled live account on a box without the shim hears about its login exactly as the default
    /// account does.
    fn report_not_running_account(&self, inputs: &AdapterInputs<'_>) {
        login(inputs).report_live_tier(VENUE, inputs.account);
    }

    /// The account's resting orders, every pair's, read on its RUNNING reconcile session (`crate::reconcile::venue_read`): no
    /// login of its own, and no PRICE or TRIGGER (the shim's Orders row has no rate). The balance and the positions
    /// are not offered.
    fn venue_read(
        &self,
        inputs: &AdapterInputs<'_>,
        kind: vike_model::VenueReadKind,
    ) -> Result<vike_model::VenueRead, vike_utility::errors::CarriedRefusal> {
        crate::reconcile::venue_read::venue_read(inputs.account, kind)
    }

    /// **The account's ForexConnect session is SUPERVISED** (decision 0164 verdict 7(b), amended 2026-11-21, owner's Q4):
    /// the exec thread logs in only until its first session opens, so a session the SDK gave up on (`SessionLost`) is
    /// rebuilt by the gatewayhub, at the FX open while the market is closed. The probe reads the cell the exec thread writes,
    /// by the account's route key (`crate::account::venue_link::FxcmSessionProbe`): no session, thread or SDK call of its own.
    fn link_probe(
        &self,
        _inputs: &AdapterInputs<'_>,
        route_key: &str,
    ) -> Option<Box<dyn vike_bridge_core::venue_link::VenueLinkProbe>> {
        Some(Box::new(crate::account::venue_link::FxcmSessionProbe::new(venue_link(route_key))))
    }
}

#[path = "adapter_tests.rs"]
#[cfg(test)]
mod adapter_tests;
