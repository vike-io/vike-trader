//! cTrader's VENUE MOUNT — the `vike_bridge_core::venue_mount::VenueMount` contract
//! ([`CtraderVenueMount`]) over the live-mount seam this module has held since decision 0088's B5
//! step ([`live_mount_for_account`]): the OAuth credential load, the dedicated reconcile socket, the
//! blocking protobuf/TLS exec handshake and the halt-admit wiring
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md).
//!
//! Moved, behaviour unchanged, from `vike-mount` with the venue mount contract: its `("ctrader", _)`
//! arm (now [`CtraderVenueMount::mount`], which calls [`live_mount_for_account`] exactly as the arm
//! did), its arming-probe row ([`CtraderVenueMount::resolve`]), its clock, book-identity and
//! grid-source rows ([`CtraderVenueMount::declaration`]) and its LAZY startup credential probe
//! ([`CtraderVenueMount::credential_probe`]). ONE input changed shape: the credential-rotation home
//! is the state directory the mount is HANDED, where [`live_mount_for_account`] used to read that
//! process-wide boot fact for itself.
//!
//! ## What this module still does NOT decide
//!
//! * **The arming ceiling.** `vike-mount` refuses to reach this venue at all once its ceiling is
//!   `paper`, and cTrader has no second tier switch — no `CTRADER_MAINNET`-shaped flag; the OAuth
//!   shape loaded here is always the `Demo`-tier one, whatever the ceiling (a live-token flip is a
//!   deliberate code change, not a runtime switch). Nothing here reads `live_permitted`.
//! * **The risk-grid fold.** [`CtraderVenueMount::mount`] answers the two questions the fold asks —
//!   `risk_properties(symbol)` for the mounted symbol, `risk_properties(leg)` per declared leg, both
//!   off the exec handshake's own [`SymbolMap`] at no network cost — and `vike-mount`'s shared
//!   `symbol_grid` fold turns the answers into `RiskLimits`, as it does for every venue.
//! * **The halt-admit report.** `vike-mount` says, once per mount and for every venue, whether an
//!   armed `verify` reached a live client. This module only APPLIES the resolved
//!   [`vike_model::HaltAdmit`], as a plain value.
//!
//! ## ⚠ The weaker robustness contract
//!
//! Unlike the self-healing actor spawns of the crypto venues, deribit and aster, cTrader's exec
//! handshake is BLOCKING and FALLIBLE and runs synchronously at mount. A connect failure DEMOTES the
//! venue to PAPER for the whole session (there is no in-thread reconnect), and any reconcile socket
//! already opened for the attempt is dropped with it. A follow-up should move the connect into a
//! self-healing spawn so a transient startup outage does not strand cTrader on paper.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use vike_bridge_core::credentials::Environment;
use vike_bridge_core::venue_mount::{
    BookIdentity, ClockDecl, CredentialProbe, DeclaredGridSource, ExecOutcome, HeldBelowLive,
    LiveExec, MountInputs, MountOutcome, MountRequest, PaperCause, Resolution, Tier,
    VenueDeclaration, VenueMount, recon_if_enabled,
};
use vike_exec::recon::ReconClient;
use vike_exec::{EventSender, ExecutionClient};
use vike_model::HaltAdmit;
use vike_model::account_keys::AccountLabel;

use crate::config::CtraderConfig;
use crate::conn::{VENUE, connect_and_auth_exec};
use crate::exec::CtraderExec;
use crate::recon_client::recon_client;
use crate::symbols::SymbolMap;

/// The symbol the startup credential probe scopes its reconcile client to — was `vike-mount`'s
/// `INLINE_AUTHED_READ_MARKETS` row. It mirrors `crates/vike-tradehub/src/wired_markets.rs`'s `CTRADER_MARKET`
/// (the ctrader row of `WIRED_MARKETS`), as that row did: a bridge cannot name `vike-tradehub`, so the
/// two change together. It scopes the recon client's order/position reads; the balance read the
/// probe issues is account-wide.
///
/// ⚠ No MEASURED healthy reading accompanies this probe, and that is a statement, not an
/// omission: the cTrader demo grant available on the box that added it was expired at the venue
/// (`CH_ACCESS_TOKEN_INVALID`; the alpaca+ctrader live rehearsal, PR #1407). It is proven by its
/// failure path and its offline tests; a PASS wants a run with a live grant.
const PROBE_SYMBOL: &str = "EURUSD";

/// The loader [`live_mount_for_account`] — and so [`CtraderVenueMount::mount`] — builds its config
/// through: this account's OAuth grant at the DEMO tier, the only tier this venue mounts, plus the
/// rotation home `state_dir` names.
///
/// It is NOT the arming probe. [`CtraderVenueMount::resolve`] calls
/// `CtraderConfig::from_vars_for_account`, which is this same loader with no rotation home: presence
/// is decided from `vars` alone and `state_dir` adds only that home, so the probe and the mount
/// cannot disagree about whether the venue arms.
fn grant(
    account: &AccountLabel,
    vars: &HashMap<String, String>,
    state_dir: Option<&Path>,
) -> Option<CtraderConfig> {
    CtraderConfig::from_vars_with_store_for_account(Environment::Demo, account, vars, state_dir)
}

/// A live cTrader mount: the exec client, its dedicated reconcile client (when one was requested
/// and connected), and the exec handshake's own resolved instrument grid.
pub struct CtraderMount {
    /// Boxed for the contract's `Box<dyn ExecutionClient + Send>` slot.
    pub client: Box<dyn ExecutionClient + Send>,
    /// `Some` when reconcile was requested AND the dedicated authed socket connected — opened on
    /// its OWN handshake (mirrors deribit/aster), so a reconcile read never contends the exec
    /// actor's protobuf socket.
    pub recon: Option<Box<dyn ReconClient>>,
    /// The exec handshake's own resolved symbol map, shared (an `Arc`, no further network call) so
    /// the mounted symbol's grid and every declared leg's are answered at no extra cost.
    pub symbols: Arc<SymbolMap>,
}

/// Build the live cTrader mount for one account, straight from the workspace credential map — the
/// seam [`CtraderVenueMount::mount`] calls, and the unit the `#[ignore]`d live smokes under
/// `crates/bridges/ctrader/tests/` drive directly.
///
/// Returns `None` — leaving the caller on its paper fallback — for either of the two cases that
/// were byte-identical before this factory existed:
/// * **no OAuth grant configured** for this account (`CTRADER_CLIENT_ID`/`_SECRET` +
///   `CTRADER_DEMO_ACCESS_TOKEN`/`_REFRESH_TOKEN`, this account's own names): absent credentials
///   ARE the live gate, and no network call is made;
/// * **the blocking protobuf/TLS handshake failed** (the module doc's weaker robustness contract).
///   Any reconcile socket already opened for this attempt is dropped with it — a paper venue holds
///   no reconcile handle, exactly like the absent-creds path.
///
/// `state_dir` is the credential-ROTATION home cTrader's rotating refresh token needs —
/// `<project>/settings/state`, from which the credential store and the change journal are both
/// derived. It is a PARAMETER: [`CtraderVenueMount::mount`] passes `MountInputs::process.state_dir`,
/// which `vike-mount` resolves once from the boot's declaration, and a caller that declared no
/// project passes `None` — no rotation home, so a refreshed grant lives as long as the process.
/// Until the venue mount contract this function read that process-wide boot fact itself.
pub fn live_mount_for_account(
    account: &AccountLabel,
    vars: &HashMap<String, String>,
    state_dir: Option<&Path>,
    symbol: &str,
    recon_enabled: bool,
    events: &EventSender,
    halt_admit: HaltAdmit,
) -> Option<CtraderMount> {
    let cfg = grant(account, vars, state_dir)?;
    // Reconcile FIRST, on its OWN dedicated authed socket (mirrors the deribit and aster mounts): a
    // reconcile report fetch must never contend the exec actor's protobuf socket. `None` — either
    // reconcile is off, or the handshake failed — leaves this mount reconcile-inert; exec is
    // unaffected either way. cTrader has no `PropertiesRecorder` and takes no `recon_trigger`
    // (interval-only reconcile, like deribit/aster).
    //
    // LAZY: with reconciliation off, this second protobuf/TLS OAuth handshake is never attempted —
    // `recon_if_enabled` never calls the factory.
    let recon = recon_if_enabled(recon_enabled, || recon_client(&cfg, symbol));
    // A no-op live-data sink: this mount wires exec only — market data comes from the separate
    // `CtraderData` feed the composition root wires elsewhere.
    let sink: Arc<dyn vike_data::LiveDataSink> = Arc::new(vike_data::TeeSink(Vec::new()));
    match connect_and_auth_exec(cfg.to_conn_config(), sink, events.clone()) {
        Ok(handle) => {
            tracing::warn!(
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
            let client: Box<dyn ExecutionClient + Send> =
                Box::new(CtraderExec::new(handle, events.clone()).with_halt_admit(halt_admit));
            Some(CtraderMount { client, recon, symbols })
        }
        Err(e) => {
            // Demote to PAPER for this attempt. Any recon socket opened moments ago would reconcile
            // a PAPER engine against LIVE cTrader state, so it is dropped here with the rest of
            // this function's locals, never returned.
            tracing::warn!(
                error = %e,
                "ctrader: exec connect/auth failed → falling back to PAPER for this session"
            );
            None
        }
    }
}

/// [`CtraderVenueMount::mount`]'s body, with its one networked step taken as a parameter:
/// `live_mount` is handed [`live_mount_for_account`]'s arguments, in that function's order, and
/// answers what it would. `mount` passes that function itself — the same call, the same arguments —
/// and a test passes a double, which is what lets `mount_contract_tests.rs` read, offline, every
/// argument this body forwards and the Live outcome it folds from a handshake:
/// `live_mount_for_account` dials `demo.ctraderapi.com` once a grant is present, which no offline
/// test may do.
fn mount_with(
    req: MountRequest<'_>,
    live_mount: impl FnOnce(
        &AccountLabel,
        &HashMap<String, String>,
        Option<&Path>,
        &str,
        bool,
        &EventSender,
        HaltAdmit,
    ) -> Option<CtraderMount>,
) -> MountOutcome {
    let Some(mount) = live_mount(
        req.inputs.account,
        req.inputs.secrets,
        req.inputs.process.state_dir.as_deref(),
        req.symbol,
        req.recon_enabled,
        req.events,
        req.halt_admit,
    ) else {
        // Absent/half-written credentials, or a failed handshake: paper, nothing reconciled.
        return MountOutcome::paper();
    };
    // Live RiskGate from the handshake-resolved grid (PR-2b): no extra network. An unknown symbol
    // keeps the permissive default, same as the crypto arms' failed pre-fetch.
    let grid = mount.symbols.risk_properties(req.symbol);
    // …and the SAME table answers for every DECLARED LEG at one map lookup each; a leg it does not
    // know gets no row, which `vike-mount`'s fold then reports as ungridded. The fold applies its own
    // leg rules (the mounted symbol and blank legs get no row) over these answers.
    let leg_grids = req
        .declared_legs
        .iter()
        .filter_map(|leg| mount.symbols.risk_properties(leg).map(|p| (leg.clone(), p)))
        .collect();
    MountOutcome {
        exec: ExecOutcome::Live(LiveExec {
            client: mount.client,
            bound_tier: Tier::Demo,
            grid,
            contract_size: None,
            margin_mode: None,
            leg_grids,
        }),
        recon: mount.recon,
        identity: None,
    }
}

/// cTrader's mount. `vike_tradehub::registry::REGISTRY` holds `&CtraderVenueMount`.
pub struct CtraderVenueMount;

impl VenueMount for CtraderVenueMount {
    fn venue(&self) -> &'static str {
        VENUE
    }

    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration {
            addresses_accounts: true,
            process_exclusive: None,
            // Interval-only reconcile: `vike_mount::build_node` threads no trigger for this venue.
            takes_recon_trigger: false,
            // The mount reads `symbols.risk_properties(symbol)` off the exec handshake's own
            // `SymbolsList` — `crates/bridges/ctrader/src/symbols.rs`'s `risk_properties`, whose own
            // doc says "Needs NO network" — so a declared leg's grid costs one map lookup.
            grid_source: DeclaredGridSource::InHand,
            // `crates/bridges/ctrader/src/config.rs`: `ACCOUNT_ID` is OPTIONAL — when absent the
            // account is DISCOVERED at connect. So the row is determinable exactly when the
            // operator wrote it, and `effective_book`'s `None` covers the rest: a row cannot be
            // "sometimes undeterminable" as a CLASSIFICATION, but a value can be absent.
            book_identity: BookIdentity::Named {
                prefix: "CTRADER",
                demo_tiers: &["DEMO"],
                live_tiers: &["LIVE"],
                name_suffixes: &["ACCOUNT_ID"],
                evm_key_suffixes: &[],
            },
            // The absence is a property of the PUBLISHED SCHEMA, not of our wiring:
            // `crates/bridges/ctrader/proto/OpenApiCommonMessages.proto` and `OpenApiMessages.proto`
            // hold no message, field or rpc matching /server ?time/; `ProtoHeartbeatEvent` carries
            // only `payload_type`; and `ProtoOaSpotEvent`'s optional `timestamp` is a TICK stamp
            // (hours or days stale on a closed market) that `crates/bridges/ctrader/src/conn.rs`'s
            // `write_command` does not even request.
            clock: ClockDecl::NotWired {
                reason: "the Open API protobuf schema publishes no server time at all — the heartbeat \
                         carries none and the only timestamp on the wire is a market tick",
                unmeasured_risk: None,
            },
        }
    }

    fn resolve(&self, inputs: &MountInputs<'_>) -> Resolution {
        if CtraderConfig::from_vars_for_account(Environment::Demo, inputs.account, inputs.secrets)
            .is_some()
        {
            Resolution::Armed {
                tier: Tier::Demo,
                held_below_live: Some(HeldBelowLive::DemoOnlyArm),
            }
        } else {
            Resolution::Paper(PaperCause::NoCredentials)
        }
    }

    fn mount(&self, req: MountRequest<'_>) -> MountOutcome {
        mount_with(req, live_mount_for_account)
    }

    /// Was `vike-mount`'s `authed_read_probes` ctrader branch, unchanged — LAZY, and the laziness is
    /// the point: `recon_client` opens and AUTHENTICATES a protobuf socket during construction (the
    /// venue has no cheaper authed read), so building it here would turn a refused grant into
    /// `None` → no probe → NO ROW, the silent absence the credential leg exists to remove. Deferred
    /// into the closure, a refused handshake is the leg's FAIL. The DEFAULT account's grant, as the
    /// branch read it; no rotation home (the probe refreshes nothing it keeps).
    fn credential_probe(&self, inputs: &MountInputs<'_>) -> Option<CredentialProbe> {
        let cfg = CtraderConfig::from_vars(Environment::Demo, inputs.secrets)?;
        let symbol = PROBE_SYMBOL.to_string();
        let endpoint = format!("{}:{}", cfg.host, cfg.port);
        Some(CredentialProbe::ReadOnly(Arc::new(move || {
            match recon_client(&cfg, &symbol) {
                Some(client) => client
                    .fetch_balance()
                    .map(|_| ())
                    .map_err(|e| format!("{e} (authenticated protobuf socket {endpoint})")),
                // The handshake is where cTrader checks the grant, so this IS the credential
                // verdict — an expired `CTRADER_DEMO_ACCESS_TOKEN` lands here.
                None => Err(format!(
                    "connect/auth refused at {endpoint} — check reachability and the                          CTRADER_DEMO_ACCESS_TOKEN/_REFRESH_TOKEN grant (re-issue it with                          `{}`)",
                    crate::token_store::REAUTHORIZE_CMD
                )),
            }
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Absent credentials ⇒ `None` and **no network call** — the CI-safe half, mirroring
    /// `vike_polymarket::exec_plane::mount`'s `without_a_usable_key_the_mount_is_none_and_offline`: this test
    /// must return before `connect_and_auth_exec` ever dials a socket.
    #[test]
    fn without_credentials_the_mount_is_none_and_offline() {
        let (tx, _rx) = vike_exec::event_channel(8);
        let vars = HashMap::new();
        assert!(
            live_mount_for_account(
                &AccountLabel::Default,
                &vars,
                None,
                "EURUSD",
                true,
                &tx,
                HaltAdmit::default()
            )
            .is_none()
        );
    }

    /// A half-written grant (app pair present, tier tokens absent) is the same live gate as no
    /// grant at all — still no network call.
    #[test]
    fn a_half_written_grant_is_also_none_and_offline() {
        let (tx, _rx) = vike_exec::event_channel(8);
        let mut vars = HashMap::new();
        vars.insert("CTRADER_CLIENT_ID".to_string(), "app-id".to_string());
        vars.insert("CTRADER_CLIENT_SECRET".to_string(), "app-secret".to_string());
        assert!(
            live_mount_for_account(
                &AccountLabel::Default,
                &vars,
                None,
                "EURUSD",
                true,
                &tx,
                HaltAdmit::default()
            )
            .is_none()
        );
    }
}

#[path = "mount_contract_tests.rs"]
#[cfg(test)]
mod mount_contract_tests;
