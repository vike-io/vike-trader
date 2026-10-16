//! cTrader's VENUE ADAPTER — the `vike_bridge_core::venue_adapter::VenueAdapter` contract
//! ([`CtraderVenueAdapter`]) over the live-mount seam this module has held since decision 0088's B5
//! step ([`live_runtime_for_account`]): the OAuth credential load, the dedicated reconcile socket, the
//! blocking protobuf/TLS exec handshake and the halt-admit wiring
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md).
//!
//! Moved, behaviour unchanged, from `vike-tradehub-core` with the venue-adapter contract: its `("ctrader", _)`
//! arm (now [`CtraderVenueAdapter::start`], which calls [`live_runtime_for_account`] exactly as the arm
//! did), its arming-probe row ([`CtraderVenueAdapter::resolve`]), its clock, book-identity and
//! grid-source rows ([`CtraderVenueAdapter::declaration`]) and its LAZY startup credential probe
//! ([`CtraderVenueAdapter::credential_probe`]). ONE input changed shape: the credential-rotation home
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
//! * **The risk-grid fold.** [`CtraderVenueAdapter::start`] answers the two questions the fold asks —
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
//! handshake is BLOCKING and FALLIBLE and runs synchronously at start. A connect failure DEMOTES the
//! venue to PAPER for the whole session (there is no in-thread reconnect), and no reconcile socket
//! is dialled for the attempt (the dedicated socket opens only once exec is up). A follow-up should
//! move the connect into a self-healing spawn so a transient startup outage does not strand cTrader
//! on paper.
//!
//! ⚠ **A DEMO-ONLY ARM, and a LIVE account is NAMED, not ignored.** This arm dials only the demo
//! host (`demo.ctraderapi.com`; a live-token flip is a deliberate code change, not a runtime
//! switch). A `demo` account holding its complete grant arms DEMO, held below live as
//! `HeldBelowLive::DemoOnlyArm`. A `live` account holding its complete grant answers
//! [`PaperCause::LiveTierNotWired`] — never `NoCredentials`, the words of an empty store — and
//! `start` says so ONCE at `error!` (`vike_bridge_core::venue_adapter::report_live_tier_not_wired`)
//! and returns paper BEFORE the live-mount step, which would dial a socket: no live account's grant
//! is ever sent to this venue from this build. The same function speaks for a labelled account that
//! is never started ([`VenueAdapter::report_not_running_account`]). An incomplete grant, at either
//! tier, is the ordinary silent `NoCredentials`.

use std::path::Path;
use std::sync::Arc;

use vike_bridge_core::venue_adapter::{
    AdapterInputs, AdapterOutcome, AdapterRequest, BookIdentity, ClockDecl, CredentialProbe,
    DeclaredGridSource, ExecOutcome, HeldBelowLive, LiveExec, PaperCause, Resolution, VenueAdapter,
    VenueDeclaration, recon_if_enabled, report_live_tier_not_wired,
};
use vike_exec::recon::ReconClient;
use vike_exec::{EventSender, ExecutionClient};
use vike_model::AccountTier;
use vike_model::HaltAdmit;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_credentials::AccountCredentials;

use crate::config::{ACCOUNT_ID, CtraderConfig, demo_arm};
use crate::conn::{VENUE, connect_and_auth_exec};
use crate::exec::CtraderExec;
use crate::recon_client::recon_client;
use crate::symbols::SymbolMap;

/// The symbol the startup credential probe scopes its reconcile client to — was `vike-tradehub-core`'s
/// `INLINE_AUTHED_READ_MARKETS` row.
/// It is this venue's row of `crates/vike-venues/src/venue_wiring.rs`'s `VENUE_WIRING` — the
/// symbol the daemon's default engine starts on — read at compile time, so the probe cannot drift
/// from the engine market and a row that loses its engine fails this crate's build. It scopes the recon client's order/position reads; the balance read the
/// probe issues is account-wide.
///
/// ⚠ No MEASURED healthy reading accompanies this probe, and that is a statement, not an
/// omission: the cTrader demo grant available on the box that added it was expired at the venue
/// (`CH_ACCESS_TOKEN_INVALID`; the alpaca+ctrader live rehearsal, PR #1407). It is proven by its
/// failure path and its offline tests; a PASS wants a run with a live grant.
const PROBE_SYMBOL: &str = match vike_venues::venue_wiring::wired_symbol(VENUE) {
    Some(symbol) => symbol,
    None => {
        panic!("this venue's credential probe needs an engine row in vike_model's VENUE_WIRING")
    }
};

/// The word this arm uses for the one tier it starts, spoken by `report_live_tier_not_wired`.
const DEMO_WORD: &str = "demo";

/// The loader [`live_runtime_for_account`] — and so [`CtraderVenueAdapter::start`] — builds its config
/// through: this account's OAuth grant on the DEMO host, the only host this venue dials, plus the
/// rotation home `state_dir` names.
///
/// It is also the arming probe's completeness test ([`resolution_for`], with no rotation home):
/// presence is decided from the credentials alone and `state_dir` adds only that home, so the probe
/// and the start cannot disagree about whether the grant is complete.
fn grant(credentials: &AccountCredentials<'_>, state_dir: Option<&Path>) -> Option<CtraderConfig> {
    CtraderConfig::from_credentials_with_store(AccountTier::Demo, credentials, state_dir)
}

/// **THE arming answer for the account `inputs` names** — [`CtraderVenueAdapter::resolve`] IS this,
/// and `start` / `report_not_running_account` act on it, so the three cannot disagree. The tier is
/// judged by `crate::config`'s `demo_arm` (exhaustive there; `Paper` reads as demo, never as live).
fn resolution_for(inputs: &AdapterInputs<'_>) -> Resolution {
    let complete = grant(&inputs.credentials, None).is_some();
    match (demo_arm(inputs.tier), complete) {
        (true, true) => Resolution::Armed {
            tier: AccountTier::Demo,
            held_below_live: Some(HeldBelowLive::DemoOnlyArm),
        },
        // A live account holding its grant: present and unusable, a NAMED cause — this arm never
        // dials the live host.
        (false, true) => Resolution::Paper(PaperCause::LiveTierNotWired),
        (_, false) => Resolution::Paper(PaperCause::NoCredentials),
    }
}

/// **Everything this arm says about a LIVE account**, in one place: `start` calls it for the
/// account it is asked to start and [`VenueAdapter::report_not_running_account`] for a labelled account
/// that is never started, so the two cannot word one fact two ways. Says the one
/// `report_live_tier_not_wired` line exactly when [`resolution_for`] answers
/// [`PaperCause::LiveTierNotWired`], nothing otherwise, and answers whether it spoke.
fn say_the_live_tier_is_not_wired(inputs: &AdapterInputs<'_>) -> bool {
    let live = resolution_for(inputs) == Resolution::Paper(PaperCause::LiveTierNotWired);
    if live {
        report_live_tier_not_wired(VENUE, inputs.account, DEMO_WORD);
    }
    live
}

/// A live cTrader runtime: the exec client, its dedicated reconcile client (when one was requested
/// and connected), and the exec handshake's own resolved instrument grid.
pub struct CtraderLiveRuntime {
    /// Boxed for the contract's `Box<dyn ExecutionClient + Send>` slot.
    pub client: Box<dyn ExecutionClient + Send>,
    /// `Some` when reconcile was requested AND the dedicated authed socket connected — opened on
    /// its OWN handshake (mirrors deribit/aster), so a reconcile read never contends the exec
    /// actor's protobuf socket.
    pub recon: Option<Box<dyn ReconClient>>,
    /// The exec handshake's own resolved symbol map, shared (an `Arc`, no further network call) so
    /// the engine symbol's grid and every declared leg's are answered at no extra cost.
    pub symbols: Arc<SymbolMap>,
}

/// Build the live cTrader runtime for one account, straight from that account's credentials — the
/// seam [`CtraderVenueAdapter::start`] calls, and the unit the `#[ignore]`d live smokes under
/// `crates/bridges/ctrader/tests/` drive directly. `account` is the account's label, for the log
/// line; `credentials` are its fields and the venue's application pair.
///
/// Returns `None` — leaving the caller on its paper fallback — for either of the two cases that
/// were byte-identical before this factory existed:
/// * **no complete OAuth grant** in `credentials` (the venue's `CLIENT_ID`/`CLIENT_SECRET` and the
///   account's `ACCESS_TOKEN`/`REFRESH_TOKEN`): absent credentials ARE the live gate, and no
///   network call is made;
/// * **the blocking protobuf/TLS handshake failed** (the module doc's weaker robustness contract).
///   No reconcile socket is dialled for this attempt (it opens only once exec is up) — a paper
///   venue holds no reconcile handle, exactly like the absent-creds path.
///
/// It dials the DEMO host whatever account it is handed; [`CtraderVenueAdapter::start`] never hands it
/// a `live` account (the module doc's demo-only rule).
///
/// `state_dir` is the credential-ROTATION home cTrader's rotating refresh token needs —
/// `<project>/settings/state`, from which the credential store and the change journal are both
/// derived. It is a PARAMETER: [`CtraderVenueAdapter::start`] passes `AdapterInputs::process.state_dir`,
/// which `vike-tradehub-core` resolves once from the boot's declaration, and a caller that declared no
/// project passes `None` — no rotation home, so a refreshed grant lives as long as the process.
/// Until the venue-adapter contract this function read that process-wide boot fact itself.
///
/// `halt_path` is the operator HALT sentinel the exec client watches — `AdapterInputs::process.halt_path`,
/// the one file `vike-tradehub-core` resolved for the whole process — handed to the client with
/// `CtraderExec::with_halt_path`. It is a parameter for the reason `state_dir` is: this function
/// resolves no process-global path (decision 0099). A caller with no sentinel — a smoke — passes a
/// path it owns and never creates.
pub fn live_runtime_for_account(
    account: &AccountLabel,
    credentials: &AccountCredentials<'_>,
    state_dir: Option<&Path>,
    symbol: &str,
    recon_enabled: bool,
    events: &EventSender,
    halt_admit: HaltAdmit,
    halt_path: &Path,
) -> Option<CtraderLiveRuntime> {
    let cfg = grant(credentials, state_dir)?;
    // A no-op live-data sink: this start wires exec only — market data comes from the separate
    // `CtraderData` feed the composition root wires elsewhere.
    let sink: Arc<dyn vike_data::LiveDataSink> = Arc::new(vike_data::TeeSink(Vec::new()));
    match connect_and_auth_exec(cfg.to_conn_config(), sink, events.clone()) {
        Ok(handle) => {
            // Reconcile on its OWN dedicated authed socket (mirrors the deribit and aster starts):
            // a reconcile report fetch must never contend the exec actor's protobuf socket. `None`
            // — either reconcile is off, or the handshake failed — leaves this start
            // reconcile-inert; exec is unaffected either way. cTrader has no `PropertiesRecorder`
            // and takes no `recon_trigger` (interval-only reconcile, like deribit/aster).
            //
            // ⚠ AFTER the exec handshake, and that is deliberate: a grant that had lapsed before
            // this start is healed (refreshed and saved) by the exec connect, and a reconcile
            // client dialled FIRST used the lapsed pair, was refused and left the venue
            // reconcile-less for the whole session. Dialled now, it joins the process's shared
            // grant (`CtraderConfig::to_conn_config`) and authenticates with the healed pair. A
            // failed exec connect no longer leaves a reconcile socket opened and dropped either.
            //
            // LAZY: with reconciliation off, this second protobuf/TLS OAuth handshake is never
            // attempted — `recon_if_enabled` never calls the factory.
            let recon = recon_if_enabled(recon_enabled, || recon_client(&cfg, symbol));
            tracing::warn!(
                venue = VENUE,
                account = %account,
                tier = AccountTier::Demo.as_str(),
                "ctrader: DEMO credentials present → LIVE exec client (real demo orders)"
            );
            // The exec handshake's own resolved grid, cloned (an `Arc`, no network) BEFORE `handle`
            // is consumed below.
            let symbols = handle.symbols.clone();
            // THE halt-admit consumer. cTrader is the ONE venue where `HaltAdmit::Verify` does
            // anything (`vike_model::halt_verify_support`), because it is the only adapter holding
            // a position book at its halt boundary — seeded at connect, kept fresh from every
            // execution event. `Admit` (the default, and every deployment with no `policy.toml`) is
            // byte-identical to the flag-trusting rule cTrader got in #1180.
            let client: Box<dyn ExecutionClient + Send> = Box::new(
                CtraderExec::new(handle, events.clone())
                    .with_halt_admit(halt_admit)
                    .with_halt_path(halt_path.to_path_buf()),
            );
            Some(CtraderLiveRuntime { client, recon, symbols })
        }
        Err(e) => {
            // Demote to PAPER for this attempt. No reconcile socket was opened for it (it is dialled
            // only once exec is up, above): one would reconcile a PAPER engine against LIVE cTrader
            // state, and a paper venue holds no reconcile handle.
            tracing::warn!(
                error = %e,
                "ctrader: exec connect/auth failed → falling back to PAPER for this session"
            );
            None
        }
    }
}

/// [`CtraderVenueAdapter::start`]'s body, with its one networked step taken as a parameter:
/// `live_runtime` is handed [`live_runtime_for_account`]'s arguments, in that function's order, and
/// answers what it would. `start` passes that function itself — the same call, the same arguments —
/// and a test passes a double, which is what lets `adapter_contract_tests.rs` read, offline, every
/// argument this body forwards and the Live outcome it folds from a handshake:
/// `live_runtime_for_account` dials `demo.ctraderapi.com` once a grant is present, which no offline
/// test may do.
fn start_with(
    req: AdapterRequest<'_>,
    live_runtime: impl FnOnce(
        &AccountLabel,
        &AccountCredentials<'_>,
        Option<&Path>,
        &str,
        bool,
        &EventSender,
        HaltAdmit,
        &Path,
    ) -> Option<CtraderLiveRuntime>,
) -> AdapterOutcome {
    // A LIVE account is decided HERE, before the live-mount step, because that step dials a socket
    // the moment a grant is present: this arm never dials a live account's grant. Holding its grant
    // it is the named `LiveTierNotWired` (said once, at `error!`); without one it is the ordinary
    // silent paper.
    if !demo_arm(req.inputs.tier) {
        say_the_live_tier_is_not_wired(&req.inputs);
        return AdapterOutcome::paper();
    }
    let Some(runtime) = live_runtime(
        req.inputs.account,
        &req.inputs.credentials,
        req.inputs.process.state_dir.as_deref(),
        req.symbol,
        req.recon_enabled,
        req.events,
        req.halt_admit,
        &req.inputs.process.halt_path,
    ) else {
        // Absent/half-written credentials, or a failed handshake: paper, nothing reconciled.
        return AdapterOutcome::paper();
    };
    // Live RiskGate from the handshake-resolved grid (PR-2b): no extra network. An unknown symbol
    // keeps the permissive default, same as the crypto arms' failed pre-fetch.
    let grid = runtime.symbols.risk_properties(req.symbol);
    // …and the SAME table answers for every DECLARED LEG at one map lookup each; a leg it does not
    // know gets no row, which `vike-tradehub-core`'s fold then reports as ungridded. The fold applies its own
    // leg rules (the engine symbol and blank legs get no row) over these answers.
    let leg_grids = req
        .declared_legs
        .iter()
        .filter_map(|leg| runtime.symbols.risk_properties(leg).map(|p| (leg.clone(), p)))
        .collect();
    AdapterOutcome {
        exec: ExecOutcome::Live(LiveExec {
            client: runtime.client,
            bound_tier: AccountTier::Demo,
            grid,
            contract_size: None,
            margin_mode: None,
            leg_grids,
        }),
        recon: runtime.recon,
        identity: None,
    }
}

/// cTrader's start. `vike_tradehub::registry::REGISTRY` holds `&CtraderVenueAdapter`.
pub struct CtraderVenueAdapter;

impl VenueAdapter for CtraderVenueAdapter {
    fn venue(&self) -> &'static str {
        VENUE
    }

    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration {
            addresses_accounts: true,
            process_exclusive: None,
            // Interval-only reconcile: `vike_tradehub_core::assemble_runtime` threads no trigger for this venue.
            takes_recon_trigger: false,
            // The start reads `symbols.risk_properties(symbol)` off the exec handshake's own
            // `SymbolsList` — `crates/bridges/ctrader/src/symbols.rs`'s `risk_properties`, whose own
            // doc says "Needs NO network" — so a declared leg's grid costs one map lookup.
            grid_source: DeclaredGridSource::InHand,
            // `crates/bridges/ctrader/src/config.rs`: `ACCOUNT_ID` is OPTIONAL — when absent the
            // account is DISCOVERED at connect. So the row is determinable exactly when the
            // operator wrote it, and `effective_book`'s `None` covers the rest: a row cannot be
            // "sometimes undeterminable" as a CLASSIFICATION, but a value can be absent.
            book_identity: BookIdentity::Named { name_fields: &[ACCOUNT_ID], evm_key_fields: &[] },
            // The absence is a property of the PUBLISHED SCHEMA, not of our wiring:
            // `crates/bridges/ctrader/proto/OpenApiCommonMessages.proto` and `OpenApiMessages.proto`
            // hold no message, field or rpc matching /server ?time/; `ProtoHeartbeatEvent` carries
            // only `payload_type`; and `ProtoOaSpotEvent`'s optional `timestamp` is a TICK stamp
            // (hours or days stale on a closed market) that `crates/bridges/ctrader/src/conn/outbound.rs`'s
            // `write_command` does not even request.
            clock: ClockDecl::NotWired {
                reason: "the Open API protobuf schema publishes no server time at all — the heartbeat \
                         carries none and the only timestamp on the wire is a market tick",
                unmeasured_risk: None,
            },
        }
    }

    fn resolve(&self, inputs: &AdapterInputs<'_>) -> Resolution {
        resolution_for(inputs)
    }

    fn start(&self, req: AdapterRequest<'_>) -> AdapterOutcome {
        start_with(req, live_runtime_for_account)
    }

    /// A labelled account is started only when it armed, so a `live` one holding its grant never
    /// reaches `start`; `vike-tradehub-core` asks for what `start` would have said (see the trait method).
    fn report_not_running_account(&self, inputs: &AdapterInputs<'_>) {
        say_the_live_tier_is_not_wired(inputs);
    }

    /// Was `vike-tradehub-core`'s `authed_read_probes` ctrader branch — LAZY, and the laziness is the
    /// point: `recon_client` opens and AUTHENTICATES a protobuf socket during construction (the
    /// venue has no cheaper authed read), so building it here would turn a refused grant into
    /// `None` → no probe → NO ROW, the silent absence the credential leg exists to remove. Deferred
    /// into the closure, a refused handshake is the leg's FAIL. The account `vike-tradehub-core` asks about
    /// (`AdapterInputs::credentials`); no rotation home (the probe refreshes nothing it keeps). A
    /// `live` account is never probed: this arm never dials a live account's grant.
    fn credential_probe(&self, inputs: &AdapterInputs<'_>) -> Option<CredentialProbe> {
        if !demo_arm(inputs.tier) {
            return None;
        }
        let cfg = CtraderConfig::from_credentials(AccountTier::Demo, &inputs.credentials)?;
        let symbol = PROBE_SYMBOL.to_string();
        let endpoint = format!("{}:{}", cfg.host, cfg.port);
        Some(CredentialProbe::ReadOnly(Arc::new(move || {
            match recon_client(&cfg, &symbol) {
                Some(client) => client
                    .fetch_balance()
                    .map(|_| ())
                    .map_err(|e| format!("{e} (authenticated protobuf socket {endpoint})")),
                // The handshake is where cTrader checks the grant, so this IS the credential
                // verdict — an expired `ACCESS_TOKEN` lands here.
                None => Err(format!(
                    "connect/auth refused at {endpoint} — check reachability and the account's \
                     ACCESS_TOKEN/REFRESH_TOKEN grant (re-issue it with `{}`)",
                    crate::token_store::REAUTHORIZE_CMD
                )),
            }
        })))
    }
}

#[cfg(test)]
mod tests {
    use vike_secrets::account_credentials::CredentialFields;

    use super::*;

    /// Absent credentials ⇒ `None` and **no network call** — the CI-safe half, mirroring
    /// `vike_polymarket::exec_plane::adapter`'s `without_a_usable_key_the_start_is_none_and_offline`: this test
    /// must return before `connect_and_auth_exec` ever dials a socket.
    #[test]
    fn without_credentials_the_start_is_none_and_offline() {
        let (tx, _rx) = vike_exec::event_channel(8);
        assert!(
            live_runtime_for_account(
                &AccountLabel::Default,
                &AccountCredentials::none(),
                None,
                "EURUSD",
                true,
                &tx,
                HaltAdmit::default(),
                Path::new("never-built-so-never-watched")
            )
            .is_none()
        );
    }

    /// A half-written grant (the venue's app pair present, the account's tokens absent) is the same
    /// live gate as no grant at all — still no network call.
    #[test]
    fn a_half_written_grant_is_also_none_and_offline() {
        let (tx, _rx) = vike_exec::event_channel(8);
        let account = CredentialFields::new();
        let venue = CredentialFields::from_pairs([
            ("CLIENT_ID", "app-id"),
            ("CLIENT_SECRET", "app-secret"),
        ]);
        assert!(
            live_runtime_for_account(
                &AccountLabel::Default,
                &AccountCredentials::new(Some(1), Some(&account), Some(&venue)),
                None,
                "EURUSD",
                true,
                &tx,
                HaltAdmit::default(),
                Path::new("never-built-so-never-watched")
            )
            .is_none()
        );
    }
}

#[path = "adapter_contract_tests.rs"]
#[cfg(test)]
mod adapter_contract_tests;
