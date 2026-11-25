//! cTrader's VENUE ADAPTER — the `vike_bridge_core::venue_adapter::VenueAdapter` contract
//! ([`VenueAdapterCtrader`]) over the live-mount seam this module has held since decision 0088's B5
//! step ([`live_runtime_for_account`]): the OAuth credential load, the dedicated reconcile socket, the
//! blocking protobuf/TLS exec handshake and the halt-admit wiring
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md).
//!
//! Moved, behaviour unchanged, from `vike-tradehub-core` with the venue-adapter contract: its `("ctrader", _)`
//! arm (now [`VenueAdapterCtrader::start`], which calls [`live_runtime_for_account`] exactly as the arm
//! did), its arming-probe row ([`VenueAdapterCtrader::resolve`]), its clock, book-identity and
//! grid-source rows ([`VenueAdapterCtrader::declaration`]) and its LAZY startup credential probe
//! ([`VenueAdapterCtrader::credential_probe`]). ONE input changed shape: the credential-rotation home
//! is the state directory the start is HANDED, where [`live_runtime_for_account`] used to read that
//! process-wide boot fact for itself.
//!
//! ## The account and its credentials (decision 0156 item 6)
//!
//! The start reads `AdapterInputs::credentials` — the ONE account row `vike-tradehub-core` resolved for
//! `(ctrader, label, tier)`: its grant fields and the venue's application pair — and never composes
//! a credential name. `AdapterInputs::tier` is that row's tier, and it is the only tier there is: the
//! account has ONE key set, so there is no demo set beside a live one to fall back to.
//!
//! ## What this module still does NOT decide
//!
//! * **Which account, or whether it is active.** `vike-tradehub-core` refuses to reach this venue at all
//!   for an account whose tier is `paper` (or that is inactive), and resolves the account row
//!   before this module is asked.
//! * **The risk-grid fold.** [`VenueAdapterCtrader::start`] answers the two questions the fold asks —
//!   `risk_properties(symbol)` for the engine symbol, `risk_properties(leg)` per declared leg, both
//!   off the exec handshake's own [`SymbolMap`] at no network cost — and `vike-tradehub-core`'s shared
//!   `symbol_grid` fold turns the answers into `RiskLimits`, as it does for every venue.
//! * **The halt-admit report.** `vike-tradehub-core` says, once per start and for every venue, whether an
//!   armed `verify` reached a live client. This module only APPLIES the resolved
//!   [`vike_model::HaltAdmit`], as a plain value.
//!
//! ## ⚠ The weaker robustness contract
//!
//! Unlike the self-healing actor spawns of the crypto venues, deribit and aster, cTrader's exec
//! handshake is BLOCKING and FALLIBLE and runs synchronously at start (there is no in-thread
//! reconnect), and no reconcile socket is dialled for the attempt (the dedicated socket opens only
//! once exec is up). A failed connect does NOT RUN, by its kind
//! (`crate::transport::connection::ConnectionError::kind`, folded by `AdapterOutcome::failed`): one
//! that never reached the venue (a network failure, a timeout, the venue's own "not now", a refused
//! token whose OAuth refresh could not reach the token endpoint) is `Unreachable` and the tradehub
//! asks again until it starts (owner, 2026-11-16); a refusal (a refused token the OAuth endpoint
//! would not renew either, a refused application, an unknown failure) parks until the operator acts.
//!
//! ⚠ **A DEMO-ONLY ARM, and a LIVE account is NAMED, not ignored.** This arm dials only the demo
//! addresses (the `ctrader demo` rows of `venue_url`, `crate::config::ctrader_host` at `Demo`; a
//! live-token flip is a deliberate code change, not a runtime switch). A demo row the table cannot
//! answer is the start's refusal, said at `error!`: the account does NOT RUN. A `demo` account holding its complete grant arms DEMO, held below live as
//! `ArmingHeldBelowLive::DemoOnlyArm`. A `live` account holding its complete grant answers
//! [`ArmingNotRunningCause::LiveTierNotWired`] — never `NoCredentials`, the words of an empty store — and
//! `start` says so ONCE at `error!` (`vike_bridge_core::venue_adapter::ArmingDemoOnly::report_live_tier`)
//! and returns paper BEFORE the live-mount step, which would dial a socket: no live account's grant
//! is ever sent to this venue from this build. The same function speaks for a labelled account that
//! is never started ([`VenueAdapter::report_not_running_account`]). An incomplete grant, at either
//! tier, is the ordinary silent `NoCredentials`.

use std::path::Path;
use std::sync::Arc;

use vike_bridge_core::venue_adapter::{
    AdapterInputs, AdapterOutcome, AdapterRequest, ArmingDemoOnly, ArmingNotRunningCause,
    ArmingResolution, BookIdentityRule, ClockDeclaration, CredentialProbe, VenueAdapter,
    VenueAdapterDeclaration, VenueAdapterExecutionOutcome, VenueAdapterExecutionRunning, demo_arm,
    reconcile_if_enabled, report_address_refused,
};
use vike_bridge_core::venue_link::venue_link;
use vike_execution::recon::{ReconcileClient, ReconcileFetchError};
use vike_execution::{EventSender, ExecutionClient};
use vike_model::HaltAdmit;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{AccountTier, GridSource};
use vike_secrets::account_credentials::AccountCredentials;

use crate::config::{ACCOUNT_ID, CtraderConfig, CtraderUrls, ctrader_host};
use crate::execution::ExecutionClientCtrader;
use crate::instrument::symbols::SymbolMap;
use crate::reconcile::recon_client;
use crate::refusals::{CtraderStartRefusal, CtraderVenueReadRefusal};
use crate::transport::connection::{ConnectionError, VENUE, connect_and_auth_execution};

/// **The symbol the account's own row states for its engine** (`account.symbol`; owner, 2026-11-13: none is
/// compiled in), which the credential probe and the operator's venue read scope their reconcile client to: this
/// venue's handshake resolves the symbol in the account's own list (`ReconcileClientCtrader::connect` answers no
/// client for a name the list lacks), so it cannot go without one. `Err` names the missing row; an account that
/// states none does not run (`vike_model::ArmingBlock::NoAccountSymbol`), so no probe is asked of it.
///
/// ⚠ No MEASURED healthy reading accompanies the probe, and that is a statement, not an omission: the cTrader
/// demo grant available on the box that added it was expired at the venue (`CH_ACCESS_TOKEN_INVALID`; the
/// alpaca+ctrader live rehearsal, PR #1407). It is proven by its failure path and its offline tests; a PASS wants a
/// run with a live grant.
fn account_symbol_of(inputs: &AdapterInputs<'_>) -> Result<String, CtraderStartRefusal> {
    inputs
        .accounts
        .account_row(VENUE, inputs.account, inputs.tier)
        .and_then(|row| row.symbol.clone())
        .ok_or(CtraderStartRefusal::NoAccountSymbol)
}

/// The loader [`live_runtime_for_account`] — and so [`VenueAdapterCtrader::start`] — builds its config
/// through: this account's OAuth grant at the DEMO `endpoints`, the only addresses this venue dials,
/// plus the rotation home `state_dir` names.
///
/// It is also the arming probe's completeness test ([`holds_grant`], with no rotation home):
/// presence is decided from the credentials alone and `endpoints`/`state_dir` add only where it is
/// dialled and saved, so the probe and the start cannot disagree about whether the grant is complete.
fn grant(
    credentials: &AccountCredentials<'_>,
    endpoints: &CtraderUrls,
    state_dir: Option<&Path>,
) -> Option<CtraderConfig> {
    CtraderConfig::from_credentials_with_store(endpoints, credentials, state_dir)
}

/// Whether `credentials` hold a complete grant. The addresses play no part in it.
fn holds_grant(credentials: &AccountCredentials<'_>) -> bool {
    grant(credentials, &CtraderUrls::default(), None).is_some()
}

/// **THE arming decision for the account `inputs` names**: a complete grant at a tier below `live` is
/// [`ArmingDemoOnly::Demo`], a `live` account's complete grant [`ArmingDemoOnly::LiveTierNotWired`] (present and
/// unusable, a NAMED cause: this arm never dials the live host), anything less unconfigured.
/// [`VenueAdapterCtrader::resolve`] is its [`ArmingDemoOnly::resolution`], and `start` /
/// `report_not_running_account` say its [`ArmingDemoOnly::report_live_tier`], so the three cannot disagree.
/// The tier is judged by `vike_bridge_core::venue_adapter::demo_arm` (exhaustive there; `Paper` reads as
/// demo, never as live): the start dials the demo addresses only, so a live account's grant is never sent.
fn arm(inputs: &AdapterInputs<'_>) -> ArmingDemoOnly<()> {
    ArmingDemoOnly::of(inputs.tier, holds_grant(&inputs.credentials).then_some(()))
}

/// A live cTrader runtime: the exec client, its dedicated reconcile client (when one was requested
/// and connected), and the exec handshake's own resolved instrument grid.
pub struct CtraderLiveRuntime {
    /// Boxed for the contract's `Box<dyn ExecutionClient + Send>` slot.
    pub client: Box<dyn ExecutionClient + Send>,
    /// `Some` when reconcile was requested AND the dedicated authed socket connected — opened on
    /// its OWN handshake (mirrors deribit/aster), so a reconcile read never contends the exec
    /// actor's protobuf socket.
    pub reconcile: Option<Box<dyn ReconcileClient>>,
    /// The exec handshake's own resolved symbol map, shared (an `Arc`, no further network call) so
    /// the engine symbol's grid and every declared leg's are answered at no extra cost.
    pub symbols: Arc<SymbolMap>,
}

/// Build the live cTrader runtime for one account, straight from that account's credentials — the
/// seam [`VenueAdapterCtrader::start`] calls, and the unit the `#[ignore]`d live smokes under
/// `crates/bridges/ctrader/tests/` drive directly. `account` is the account's label, for the log
/// line; `credentials` are its fields and the venue's application pair.
///
/// Returns `Err` — no client, so the account does NOT RUN (decision 0156) — in two cases, told apart:
/// * `Err(None)`: **no complete OAuth grant** in `credentials` (the venue's `CLIENT_ID`/`CLIENT_SECRET`
///   and the account's `ACCESS_TOKEN`/`REFRESH_TOKEN`): absent credentials ARE the live gate, and no
///   network call is made;
/// * `Err(Some(why))`: **the blocking protobuf/TLS handshake failed** (the module doc's weaker
///   robustness contract), `why`'s [`ConnectionError::kind`] saying whether a later start can pass.
///   No reconcile socket is dialled for this attempt (it opens only once exec is up) — a venue that
///   does not run holds no reconcile handle, exactly like the absent-creds path.
///
/// It dials `endpoints` whatever account it is handed: [`VenueAdapterCtrader::start`] hands it the
/// DEMO rows (`crate::config::ctrader_host` at `Demo`) and never a `live` account (the module doc's
/// demo-only rule); a smoke hands it the seed's (`crate::test_support::demo_endpoints`).
///
/// `state_dir` is the credential-ROTATION home cTrader's rotating refresh token needs —
/// `<project>/settings/state`, from which the credential store and the change journal are both
/// derived. It is a PARAMETER: [`VenueAdapterCtrader::start`] passes `AdapterInputs::process.state_dir`,
/// which `vike-tradehub-core` resolves once from the boot's declaration, and a caller that declared no
/// project passes `None` — no rotation home, so a refreshed grant lives as long as the process.
/// Until the venue-adapter contract this function read that process-wide boot fact itself.
///
/// `halt_path` is the operator HALT sentinel the exec client watches — `AdapterInputs::process.halt_path`,
/// the one file `vike-tradehub-core` resolved for the whole process — handed to the client with
/// `ExecutionClientCtrader::with_halt_path`. It is a parameter for the reason `state_dir` is: this function
/// resolves no process-global path (decision 0099). A caller with no sentinel — a smoke — passes a
/// path it owns and never creates.
///
/// `fx_clock` is the FX login gate's clock every socket of this runtime reads (`CtraderConfig::fx_clock`):
/// [`VenueAdapterCtrader::start`] passes `AdapterInputs::fx_clock`, a smoke `None` (no gate).
pub fn live_runtime_for_account(
    account: &AccountLabel,
    credentials: &AccountCredentials<'_>,
    endpoints: &CtraderUrls,
    state_dir: Option<&Path>,
    symbol: &str,
    recon_enabled: bool,
    events: &EventSender,
    halt_admit: HaltAdmit,
    halt_path: &Path,
    fx_clock: Option<Arc<dyn vike_model::Clock + Send + Sync>>,
) -> Result<CtraderLiveRuntime, Option<ConnectionError>> {
    let Some(mut config) = grant(credentials, endpoints, state_dir) else { return Err(None) };
    config.fx_clock = fx_clock;
    // A no-op live-data sink: this start wires exec only — market data comes from the separate
    // `DataClientBusSpecCtrader` feed the composition root wires elsewhere.
    let sink: Arc<dyn vike_data::DataClientSink> =
        Arc::new(vike_data::DataClientSinkTee(Vec::new()));
    // The account's ONE path cell, claimed once here, before the exec actor can write: what an earlier start's actor
    // said is history, and that actor speaks no more. Only the exec socket states its session there.
    let mut exec_config = config.to_connection_config();
    exec_config.venue_link = events.route_key.map(|key| venue_link(&key).claim());
    match connect_and_auth_execution(exec_config, sink, events.clone()) {
        Ok(handle) => {
            // Reconcile on its OWN dedicated authed socket (mirrors the deribit and aster starts):
            // a reconcile report fetch must never contend the exec actor's protobuf socket. `None`
            // — either reconcile is off, or the handshake failed — leaves this start
            // reconcile-inert; exec is unaffected either way. cTrader has no `PropertiesRecorder`
            // and takes no `reconcile_trigger` (interval-only reconcile, like deribit/aster).
            //
            // ⚠ AFTER the exec handshake, and that is deliberate: a grant that had lapsed before
            // this start is healed (refreshed and saved) by the exec connect, and a reconcile
            // client dialled FIRST used the lapsed pair, was refused and left the venue
            // reconcile-less for the whole session. Dialled now, it joins the process's shared
            // grant (`CtraderConfig::to_connection_config`) and authenticates with the healed pair. A
            // failed exec connect no longer leaves a reconcile socket opened and dropped either.
            //
            // LAZY: with reconciliation off, this second protobuf/TLS OAuth handshake is never
            // attempted — `reconcile_if_enabled` never calls the factory.
            // The exec's book rides along: its split-exit table names a leg's reconcile fill by its exit.
            let legs = handle.positions.clone();
            let recon = reconcile_if_enabled(recon_enabled, || {
                recon_client(&config, symbol, Some(legs)).ok()
            });
            tracing::warn!(
                venue = VENUE,
                account = %account,
                tier = AccountTier::Demo.as_str(),
                "ctrader: DEMO credentials present → LIVE exec client (real demo orders)"
            );
            // The exec handshake's own resolved grid, cloned (an `Arc`, no network) BEFORE `handle`
            // is consumed below.
            let symbols = handle.symbols.clone();
            // The account's own listing, PUSHED at its login (`vike_catalog::held_list`, decision 0062's held session):
            // the map is in hand, so it costs nothing, and the datahub keeps it as the list a USER name is read
            // through (decision 0290). Before this push a mount-only account never handed its list over (audit flag
            // CTR-1: only a symbol-facts ask did), and with no kept list every order refuses.
            let listing = crate::catalog::instruments_from_map(&symbols);
            if !listing.is_empty() {
                vike_catalog::held_list::hold(VENUE, listing);
            }
            // THE halt-admit consumer. cTrader is the ONE venue where `HaltAdmit::Verify` does
            // anything (`vike_venues::venue_halt_verify::halt_verify_support`), because it is the
            // only adapter holding a position book at its halt boundary — seeded at connect, kept fresh from every
            // execution event. `Admit` (the default, and every deployment with no `policy.toml`) is
            // byte-identical to the flag-trusting rule cTrader got in #1180.
            let client: Box<dyn ExecutionClient + Send> = Box::new(
                ExecutionClientCtrader::new(handle, events.clone())
                    .with_halt_admit(halt_admit)
                    .with_halt_path(halt_path.to_path_buf()),
            );
            Ok(CtraderLiveRuntime { client, reconcile: recon, symbols })
        }
        Err(e) => {
            // The account does not run for this attempt. No reconcile socket was opened for it (it is
            // dialled only once exec is up, above), and a venue that does not run holds no reconcile handle.
            tracing::warn!(
                error = %e,
                kind = %e.kind(),
                "ctrader: exec connect/auth failed → the account does NOT RUN (a kind a later \
                 start can pass is started again by the tradehub; any other waits for the operator)"
            );
            Err(Some(e))
        }
    }
}

/// **The grid of the USER name `user` at the mount**, off the exec handshake's own map (no network): its KEPT row's
/// broker name among `rows` (the kept list; `crate::instrument::names::names_in`, decision 0290), then that name's
/// grid ([`SymbolMap::risk_properties`]). A name with no row (no ctrader list is kept yet: the account's own session
/// writes it) gets no grid, said at WARN with the translator's words, which name the fix: the start goes on with the
/// permissive default, as for a symbol the map does not list, and never dies of it.
fn mount_grid(
    symbols: &SymbolMap,
    rows: &[vike_model::Instrument],
    user: &str,
) -> Option<vike_model::SymbolProperties> {
    match crate::instrument::names::names_in(rows, user) {
        Ok(names) => symbols.risk_properties(&names.wire),
        Err(why) => {
            tracing::warn!(
                venue = VENUE,
                symbol = user,
                why = %why,
                "no grid for this symbol at the start (the permissive default stands until its kept row exists)"
            );
            None
        }
    }
}

/// [`VenueAdapterCtrader::start`]'s body, with its one networked step taken as a parameter:
/// `live_runtime` is handed [`live_runtime_for_account`]'s arguments, in that function's order, and
/// answers what it would. `start` passes that function itself — the same call, the same arguments —
/// and a test passes a double, which is what lets `adapter_contract_tests.rs` read, offline, every
/// argument this body forwards and the Live outcome it folds from a handshake:
/// `live_runtime_for_account` dials the demo trade socket once a grant is present, which no offline
/// test may do.
///
/// The addresses are looked up HERE, in the account's `AdapterInputs::settings` at `Demo`: a row the
/// table cannot answer is said once at `error!` with the refusal's words (it names the row) and the
/// account does NOT RUN; nothing is dialled. A failed handshake is folded by its kind
/// ([`ConnectionError::kind`] through `AdapterOutcome::failed`).
fn start_with(
    req: AdapterRequest<'_>,
    live_runtime: impl FnOnce(
        &AccountLabel,
        &AccountCredentials<'_>,
        &CtraderUrls,
        Option<&Path>,
        &str,
        bool,
        &EventSender,
        HaltAdmit,
        &Path,
        Option<Arc<dyn vike_model::Clock + Send + Sync>>,
    ) -> Result<CtraderLiveRuntime, Option<ConnectionError>>,
) -> AdapterOutcome {
    // A LIVE account is decided HERE, before the live-mount step, because that step dials a socket
    // the moment a grant is present: this arm never dials a live account's grant. Holding its grant
    // it is the named `LiveTierNotWired` (said once, at `error!`); without one it is the ordinary
    // silent paper.
    if !demo_arm(req.inputs.tier) {
        arm(&req.inputs).report_live_tier(VENUE, req.inputs.account);
        return AdapterOutcome::paper();
    }
    let endpoints = match ctrader_host(req.inputs.settings.urls(), AccountTier::Demo) {
        Ok(endpoints) => endpoints,
        Err(why) => {
            report_address_refused(VENUE, req.inputs.account, why);
            return AdapterOutcome::paper();
        }
    };
    let runtime = match live_runtime(
        req.inputs.account,
        &req.inputs.credentials,
        &endpoints,
        req.inputs.process.state_dir.as_deref(),
        req.symbol,
        req.reconcile_enabled,
        req.events,
        req.halt_admit,
        &req.inputs.process.halt_path,
        req.inputs.fx_clock.cloned(),
    ) {
        Ok(runtime) => runtime,
        // Absent/half-written credentials: nothing dialled, nothing reconciled; it parks.
        Err(None) => return AdapterOutcome::paper(),
        // A failed handshake (logged by the step): asked again when a later start can pass, else it parks.
        Err(Some(why)) => return AdapterOutcome::failed(why.kind()),
    };
    // Live RiskGate from the handshake-resolved grid (PR-2b): no extra network. An unknown symbol
    // keeps the permissive default, same as the crypto arms' failed pre-fetch.
    let kept = vike_catalog::kept_list(VENUE);
    let rows = kept.as_deref().map_or(&[][..], Vec::as_slice);
    let grid = mount_grid(&runtime.symbols, rows, req.symbol);
    // …and the SAME table answers for every DECLARED LEG at one map lookup each; a leg it does not
    // know gets no row, which `vike-tradehub-core`'s fold then reports as ungridded. The fold applies its own
    // leg rules (the engine symbol and blank legs get no row) over these answers.
    let leg_grids = req
        .declared_legs
        .iter()
        .filter(|leg| !leg.trim().is_empty())
        .filter_map(|leg| mount_grid(&runtime.symbols, rows, leg).map(|p| (leg.clone(), p)))
        .collect();
    AdapterOutcome {
        exec: VenueAdapterExecutionOutcome::Running(VenueAdapterExecutionRunning {
            client: runtime.client,
            bound_tier: AccountTier::Demo,
            grid,
            contract_value: None,
            margin_mode: None,
            leg_grids,
        }),
        reconcile: runtime.reconcile,
        identity: None,
    }
}

/// **The credential probe's answer when it got no client**, `endpoint` the socket it dialled, carrying
/// [`ConnectionError::kind`]'s outcome. A failure a later try can pass (DNS, a refused or reset connect, TLS, no
/// answer, and the venue's own "not now" codes, a server it cannot reach, a maintenance window, a rate limit) is not
/// `Refused`, which the preflight reads as a WARNING and never as a refused grant (owner, 2026-11-16; the "not now"
/// codes since 2026-11-17). Any other (an expired `ACCESS_TOKEN`, a refused application, a symbol the account's list
/// lacks) is the FAIL it always was.
fn probe_failure(why: Option<ConnectionError>, endpoint: &str) -> ReconcileFetchError {
    let endpoint = endpoint.to_string();
    match why {
        Some(why) if !why.kind().is_terminal() => ReconcileFetchError::new(
            why.kind().failure_outcome(),
            CtraderStartRefusal::ProbeUnanswered { endpoint, why: why.to_string() }.to_string(),
        ),
        _ => ReconcileFetchError::unclassified(
            CtraderStartRefusal::ProbeRefused { endpoint }.to_string(),
        ),
    }
}

/// cTrader's start. `vike_tradehub::registry::REGISTRY` holds `&VenueAdapterCtrader`.
pub struct VenueAdapterCtrader;

impl VenueAdapter for VenueAdapterCtrader {
    fn venue(&self) -> &'static str {
        VENUE
    }

    fn declaration(&self) -> VenueAdapterDeclaration {
        VenueAdapterDeclaration {
            addresses_accounts: true,
            process_exclusive: None,
            // Interval-only reconcile: `vike_tradehub_core::assemble_runtime` threads no trigger for this venue.
            // The start reads `symbols.risk_properties(symbol)` off the exec handshake's own
            // `SymbolsList` — `crates/bridges/ctrader/src/instrument/symbols.rs`'s `risk_properties`, whose own
            // doc says "Needs NO network" — so a declared leg's grid costs one map lookup.
            grid_source: GridSource::InHand,
            // `crates/bridges/ctrader/src/config.rs`: `ACCOUNT_ID` is OPTIONAL — when absent the
            // account is DISCOVERED at connect. So the row is determinable exactly when the
            // operator wrote it, and `effective_book`'s `None` covers the rest: a row cannot be
            // "sometimes undeterminable" as a CLASSIFICATION, but a value can be absent.
            book_identity: BookIdentityRule::Named {
                name_fields: &[ACCOUNT_ID],
                evm_key_fields: &[],
            },
            // The absence is a property of the PUBLISHED SCHEMA, not of our wiring:
            // `crates/bridges/ctrader/proto/OpenApiCommonMessages.proto` and `OpenApiMessages.proto`
            // hold no message, field or rpc matching /server ?time/; `ProtoHeartbeatEvent` carries
            // only `payload_type`; and `ProtoOaSpotEvent`'s optional `timestamp` is a TICK stamp
            // (hours or days stale on a closed market) that `crates/bridges/ctrader/src/transport/connection/outbound.rs`'s
            // `write_command` does not even request.
            clock: ClockDeclaration::NotWired {
                reason: "the Open API protobuf schema publishes no server time at all — the heartbeat \
                         carries none and the only timestamp on the wire is a market tick",
                unmeasured_risk: None,
            },
        }
    }

    fn resolve(&self, inputs: &AdapterInputs<'_>) -> ArmingResolution {
        arm(inputs).resolution()
    }

    fn start(&self, req: AdapterRequest<'_>) -> AdapterOutcome {
        start_with(req, live_runtime_for_account)
    }

    /// A labelled account is started only when it armed, so a `live` one holding its grant never
    /// reaches `start`; `vike-tradehub-core` asks for what `start` would have said (see the trait method).
    fn report_not_running_account(&self, inputs: &AdapterInputs<'_>) {
        arm(inputs).report_live_tier(VENUE, inputs.account);
    }

    /// Was `vike-tradehub-core`'s `authed_read_probes` ctrader branch — LAZY, and the laziness is the
    /// point: `recon_client` opens and AUTHENTICATES a protobuf socket during construction (the
    /// venue has no cheaper authed read), so building it here would turn a refused grant into
    /// no client → no probe → NO ROW, the silent absence the credential leg exists to remove. Deferred
    /// into the closure, a refused handshake is the leg's FAIL, and one that never reached the venue
    /// its WARNING (`probe_failure`). The account `vike-tradehub-core` asks about
    /// (`AdapterInputs::credentials`); no rotation home (the probe refreshes nothing it keeps). A
    /// `live` account is never probed: this arm never dials a live account's grant.
    fn credential_probe(&self, inputs: &AdapterInputs<'_>) -> Option<CredentialProbe> {
        if !demo_arm(inputs.tier) || !holds_grant(&inputs.credentials) {
            return None;
        }
        // A grant and no address: the leg FAILS with the refusal's words, which name the row.
        let endpoints = match ctrader_host(inputs.settings.urls(), AccountTier::Demo) {
            Ok(endpoints) => endpoints,
            Err(why) => return Some(CredentialProbe::failing(why)),
        };
        let mut config = CtraderConfig::from_credentials(&endpoints, &inputs.credentials)?;
        config.fx_clock = inputs.fx_clock.cloned();
        let symbol = match account_symbol_of(inputs) {
            Ok(symbol) => symbol,
            Err(why) => return Some(CredentialProbe::failing(why.to_string())),
        };
        let endpoint = format!("{}:{}", config.host, config.port);
        Some(CredentialProbe::ReadOnly(Arc::new(move || {
            // The handshake is where cTrader checks the grant, so building the client IS the
            // credential verdict. ⚠ NOT `fetch_balance` on top of it: that read refuses a balance the
            // engine's ledger cannot hold (a non-USD deposit), and an `Err` here would read as a
            // refused key and start the account PAPER for a fault that is not about its credentials.
            recon_client(&config, &symbol, None)
                .map(|_| ())
                .map_err(|why| probe_failure(why, &endpoint))
        })))
    }

    /// The account's pending orders, every symbol's (`crate::reconcile::venue_read`), on one short-lived socket that follows
    /// the process's grant and never spends it. Only an account this arm would start is dialled, so a `live` one
    /// is told so with nothing sent. The balance and the positions are not offered.
    fn venue_read(
        &self,
        inputs: &AdapterInputs<'_>,
        kind: vike_model::VenueReadKind,
    ) -> Result<vike_model::VenueRead, vike_utility::errors::CarriedRefusal> {
        if kind != vike_model::VenueReadKind::OpenOrders {
            return Err(vike_model::not_offered_words(VENUE, kind).into());
        }
        match arm(inputs).resolution() {
            ArmingResolution::Armed { .. } => {}
            ArmingResolution::NotRunning(ArmingNotRunningCause::LiveTierNotWired) => {
                return Err(CtraderVenueReadRefusal::LiveTier.carried());
            }
            ArmingResolution::NotRunning(_) => {
                return Err(CtraderVenueReadRefusal::NoGrant.carried());
            }
        }
        let endpoints = ctrader_host(inputs.settings.urls(), AccountTier::Demo)?;
        let mut config = CtraderConfig::from_credentials(&endpoints, &inputs.credentials)
            .ok_or_else(|| CtraderVenueReadRefusal::GrantUnread.carried())?;
        config.fx_clock = inputs.fx_clock.cloned();
        let symbol = account_symbol_of(inputs).map_err(|why| why.carried())?;
        crate::reconcile::venue_read::open_orders(&config, &symbol)
    }
}

#[cfg(test)]
mod tests {
    use vike_secrets::account_credentials::CredentialFields;

    use super::*;

    /// Absent credentials ⇒ `Err(None)` (no handshake failed) and **no network call** — the
    /// CI-safe half, mirroring `vike_polymarket::adapter`'s
    /// `without_a_usable_key_the_start_is_none_and_offline`: this test must return before
    /// `connect_and_auth_execution` ever dials a socket.
    #[test]
    fn without_credentials_the_start_is_none_and_offline() {
        let (tx, _rx) = vike_execution::event_channel(8);
        assert!(
            live_runtime_for_account(
                &AccountLabel::Default,
                &AccountCredentials::none(),
                &CtraderUrls::default(),
                None,
                "EURUSD",
                true,
                &tx,
                HaltAdmit::default(),
                Path::new("never-built-so-never-watched"),
                None
            )
            .is_err_and(|why| why.is_none())
        );
    }

    /// A half-written grant (the venue's app pair present, the account's tokens absent) is the same
    /// live gate as no grant at all — still no network call.
    #[test]
    fn a_half_written_grant_is_also_none_and_offline() {
        let (tx, _rx) = vike_execution::event_channel(8);
        let account = CredentialFields::new();
        let venue = CredentialFields::from_pairs([
            ("CLIENT_ID", "app-id"),
            ("CLIENT_SECRET", "app-secret"),
        ]);
        assert!(
            live_runtime_for_account(
                &AccountLabel::Default,
                &AccountCredentials::new(Some(1), Some(&account), Some(&venue)),
                &CtraderUrls::default(),
                None,
                "EURUSD",
                true,
                &tx,
                HaltAdmit::default(),
                Path::new("never-built-so-never-watched"),
                None
            )
            .is_err_and(|why| why.is_none())
        );
    }
}

#[path = "adapter_contract_tests.rs"]
#[cfg(test)]
mod adapter_contract_tests;
