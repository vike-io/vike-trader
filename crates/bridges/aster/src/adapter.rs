//! aster's VENUE ADAPTER — the `vike_bridge_core::venue_adapter::VenueAdapter` contract over the
//! agent-wallet (EIP-712) exec client, its reconcile client and its public server clock
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md).
//!
//! Moved from `vike-tradehub-core`: its `("aster", _)` arm (now [`VenueAdapterAster::start`]), its
//! arming-probe row ([`VenueAdapterAster::resolve`]), its clock row WITH the read (`vike-tradehub-core`'s
//! `aster_time`, now [`VenueAdapterAster::server_time_ms`]), its book-identity and grid-source rows
//! and its fallback grid, which is gone (owner, 2026-11-13: a symbol whose grid has not loaded is refused
//! in words, `crate::execution::run`).
//!
//! Aster uses agent-wallet FIELDS of the started account (`USER`, `PRIVATE_KEY`, optional
//! `SIGNER`: `crate::transport::sign`'s `load_aster_credentials`), NOT the generic `API_KEY`/`API_SECRET`
//! pair. It is SWITCHLESS — there is no `{VENUE}_MAINNET` switch for it; the network IS the
//! account's tier (`AdapterInputs::tier`): a `live` account signs on mainnet, a `demo` one on
//! testnet. ⚠ REAL MONEY: a `live`-tier account holding its pair spawns a LIVE MAINNET exec client,
//! and this venue runs against mainnet in practice because no testnet credentials are configured
//! (`crates/bridges/aster/CLAUDE.md`'s correction record). A `live`-tier account missing either
//! field does NOT RUN (`ArmingNotRunningCause::LiveCredentialsAbsent`): an account holds ONE key set, so there
//! is no second (testnet) set beside it to fall back to (decision 0156 item 6).

use std::sync::Arc;
use std::time::Duration;

use vike_binance::family::execution::rest_reads::parse_server_time;
use vike_bridge_core::reconcile_admitted::ReconcileAdmittedSymbols;
use vike_bridge_core::signer::VenueClock;
use vike_bridge_core::venue_adapter::{
    AdapterInputs, AdapterOutcome, AdapterRequest, ArmingResolution, BookIdentityRule,
    ClockAuthentication, ClockDeclaration, VenueAdapter, VenueAdapterDeclaration,
    VenueAdapterExecutionOutcome, VenueAdapterExecutionRunning, bound_tier, bounded_public_get,
    report_address_refused, report_live_credentials_absent, resolve_one_key_set,
};
use vike_execution::{EventSender, ExecutionClient};
use vike_model::{AccountTier, ClockRisk, GridSource, SymbolProperties, VenueNetwork};
use vike_secrets::credentials::{Credentials, attribution_code_from};
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlType};

use crate::transport::sign::load_aster_credentials;
use crate::transport::urls::{AsterUrls, PERP_PRODUCT, VENUE};

/// The account's key set and the network it binds — the ONE answer [`VenueAdapterAster::resolve`]
/// reports and [`VenueAdapterAster::start`] acts on. The network is `bound_tier` of the account's
/// tier (`VenueNetwork::for_tier`: `Live` → mainnet, bound `Live`; `Demo`, and `Paper` read as
/// `Demo`, → testnet, bound `Demo`), so the arming screen and the identity record cannot name
/// different tiers; the key set is the account's own pair (`None` = the live gate).
///
/// ⚠ THE BOUND-TIER FIX — the venue-adapter contract spec's Finding 1, and the migration's ONE
/// intended behaviour change. `vike-tradehub-core`'s legacy fold addressed this venue's identity record at
/// its CEX conjunct `ceiling_selects_mainnet(venue) && live_permitted`, which is `false` for
/// aster, so a LIVE aster start addressed the DEMO row. It was harmless only because
/// `crate::reconcile`'s `ReconcileClientAster` does not forward `fetch_account_identity`, so nothing
/// was ever recorded; had a one-line forward there existed, a live identity would have been
/// recorded against the demo row.
/// `ASTER_BUILDER_FEE_RATE` from the deployment's named values, as the `feeRate` text every order carries: unset or
/// blank is `"0"` (attribution only); a finite, non-negative decimal is its own trimmed text, byte for byte as
/// before; anything else is `"0"` and WARNED (naming the row, never its value: the credential store holds it),
/// where it used to ride every order's `feeRate` as typed.
fn builder_fee_rate(vars: &std::collections::HashMap<String, String>) -> String {
    let raw = vars.get("ASTER_BUILDER_FEE_RATE").map(|s| s.trim());
    let Some(raw) = raw.filter(|s| !s.is_empty()) else {
        return "0".to_string();
    };
    if raw.parse::<f64>().is_ok_and(|rate| rate.is_finite() && rate >= 0.0) {
        return raw.to_string();
    }
    tracing::warn!(
        venue = VENUE,
        "the builder fee (ASTER_BUILDER_FEE_RATE) is not a non-negative decimal rate, so it is 0 \
         (attribution only)"
    );
    "0".to_string()
}

fn resolved(inputs: &AdapterInputs<'_>) -> Option<(AccountTier, Credentials)> {
    load_aster_credentials(&inputs.credentials)
        .map(|c| (bound_tier(VenueNetwork::for_tier(inputs.tier)), c))
}

/// The network whose clock [`VenueAdapterAster::server_time_ms`] reads: the one [`resolved`] binds,
/// and testnet when nothing resolves — exactly what `vike-tradehub-core`'s `aster_time` read. The tier is
/// load-bearing: this is a `ClockRisk::SignedTimestamp` row, whose clock FAIL demotes the venue to
/// paper (`vike-tradehub-core`'s preflight `venue_disposition`), so a demo-tier account must be judged by
/// the testnet host it signs for, never by mainnet's.
///
/// `vike-tradehub-core` reads the clock once per venue, on the DEFAULT account's inputs
/// (`crates/vike-gatewayhub/src/trade/adapters/server_time.rs`'s `venue_server_time_ms_under_policy`), so the key set
/// consulted here is that account's.
fn clock_env(inputs: &AdapterInputs<'_>) -> AccountTier {
    resolved(inputs).map_or(AccountTier::Demo, |(env, _)| env)
}

/// `crate::execution`'s `spawn_with_recorder` arguments, in its order, as [`start_with`]
/// assembles them — the one value its spawn step is handed.
struct ExecutionSpawn {
    /// The account's exec hosts, resolved ONCE by [`start_with`] at its bound tier.
    urls: AsterUrls,
    creds: Credentials,
    symbol: String,
    events: EventSender,
    properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
    builder: Option<(String, String)>,
    leverage: f64,
    /// The engine's admitted-symbol list, the SAME `Arc` the reconcile client is told each admission
    /// through: the exec thread's reconnect-resync replays the history of each symbol on it.
    admitted: Arc<ReconcileAdmittedSymbols>,
    /// The account's one clock offset, the SAME one its reconcile client signs on.
    clock: VenueClock,
    /// The HALT sentinel the client watches — `AdapterInputs::process.halt_path`, handed to the
    /// client by its `with_halt_path` (decision 0099). Not an argument of `spawn_with_recorder`:
    /// it rides the builder, applied by `start`'s spawn step.
    halt_path: std::path::PathBuf,
}

/// [`VenueAdapterAster::start`]'s body, with its two NETWORK steps — the grid pre-fetch, and the exec
/// spawn whose thread dials the venue — taken as parameters. `start` passes the real two; a test
/// passes offline doubles, which is what lets `adapter_tests.rs`'s bound-tier pin read the tier THIS
/// body binds rather than what a helper beside it answers. Nothing else here reaches a socket:
/// `crate::reconcile` only constructs.
///
/// The account's four exec hosts are resolved HERE, once, from its `venue_url` rows at the bound
/// tier ([`AsterUrls::exec`]); a table that cannot answer leaves the account NOT RUNNING, logged
/// with the refusal's words, and nothing is dialled.
///
/// The account leverage it posts is `vike_bridge_core::leverage::leverage_for` over the operator's `[risk]` budget,
/// resolved ONCE here. ⚠ **NOT clamped to a venue ceiling, unlike bybit/okx** —
/// `vike_bridge_core::leverage::clamp_to_venue_cap` exists and this arm deliberately does not call it, for the same
/// reason binance does not (Aster's perp API is a Binance-fapi fork, so it inherits the gap verbatim). VERIFIED LIVE
/// 2026-08-05: `GET /fapi/v3/exchangeInfo` (735 KB, every symbol) contains ZERO case-insensitive `leverage` matches,
/// and `/fapi/v3/leverageBracket` answers `{"code":-1102, "msg":"Mandatory parameter 'nonce' was not sent…"}` — i.e.
/// a WHOLE NEW EIP-712-SIGNED request on the start path, not a field ride-along, and on a venue that starts against
/// MAINNET in practice. Its brackets are risk-tiered like binance's, so a single flat cap does not exist to be read. A
/// too-high request is still rejected by the VENUE, which `AsterExecution`'s perp start already warns of.
fn start_with(
    req: AdapterRequest<'_>,
    fetch_grid: impl FnOnce(AsterUrls, &Credentials, &str) -> Option<SymbolProperties>,
    spawn: impl FnOnce(ExecutionSpawn) -> Box<dyn ExecutionClient + Send>,
) -> AdapterOutcome {
    let Some((env, c)) = resolved(&req.inputs) else {
        report_live_credentials_absent(VENUE, req.inputs.account, req.inputs.tier);
        return AdapterOutcome::paper();
    };
    let urls = match AsterUrls::exec(req.inputs.settings.urls(), env) {
        Ok(urls) => urls,
        Err(refusal) => {
            report_address_refused(VENUE, req.inputs.account, refusal);
            return AdapterOutcome::paper();
        }
    };
    // Two lines, not one with a substituted word: the convention
    // (`crates/vike-ops/tests/venues/live_mount_line_gate.rs`) puts `⚠ REAL-MONEY: ` on a live tier and
    // on nothing else, and a tier that varies at run time cannot be spelled `tier = …` as a literal.
    if env == AccountTier::Live {
        tracing::warn!(
            venue = VENUE,
            account = %req.inputs.account,
            tier = AccountTier::Live.as_str(),
            "⚠ REAL-MONEY: aster: LIVE (mainnet) credentials present → LIVE exec client (REAL MONEY)"
        );
    } else {
        tracing::warn!(
            venue = VENUE,
            account = %req.inputs.account,
            tier = AccountTier::Demo.as_str(),
            "aster: TESTNET credentials present → LIVE exec client (testnet orders)"
        );
    }
    // Live RiskGate from the venue's REAL grid (PR-2b): one blocking PUBLIC `exchangeInfo`
    // pre-fetch (spot or USDⓈ-M perp, routed by the `.P` suffix). It DUPLICATES the exec adapter's
    // own in-thread fetch of the same grid, so two `exchangeInfo` requests at startup are expected
    // (acceptable, startup-only). On failure the permissive default stands.
    let grid = fetch_grid(urls, &c, req.symbol);
    // Reconcile handle from the account's network + agent creds (µs EIP-712 signer), so it hits the
    // SAME network the exec client does. Pure to construct, so NOT gated on `recon_enabled` —
    // exactly as the arm was. Borrows `&c` before it moves into the spawn.
    //
    // The admitted-symbol list is ONE `Arc`: this client is told each admission
    // (`ReconcileClient::admit_symbol`), and the exec thread's reconnect-resync and gap-sentinel read the
    // same list, so a symbol the engine admits is also one whose missed fills are replayed (decision
    // 0174's reopener).
    let admitted = Arc::<ReconcileAdmittedSymbols>::default();
    // One clock offset too: the exec thread reads it at its start, and this client signs on it.
    let clock = VenueClock::default();
    let recon = crate::reconcile::reconcile_client_sharing(
        urls,
        &c,
        req.symbol,
        Arc::clone(&admitted),
        &clock,
    );
    // Aster Code (unified cross-venue attribution): `attribution_code_from` validates the address
    // against `aster`'s `AttributionMechanic::SignedBuilder`; absent/invalid degrades to `None` —
    // no `builder`/`feeRate` on the wire at all. The fee defaults to `"0"` (attribution-only, no
    // `approveBuilder` grant needed — see `crate::execution::rest::perp::VenueRestAsterPerp::approve_builder`'s doc)
    // unless `ASTER_BUILDER_FEE_RATE` is set AND the operator has separately run the one-time
    // approval.
    let builder = attribution_code_from(req.inputs.secrets, VENUE)
        .map(|address| (address, builder_fee_rate(req.inputs.secrets)));
    // ACCOUNT LEVERAGE from the operator's `[risk]` budget; unset ⇒ the historical 2.0. ⚠ A
    // `live`-tier account signs on mainnet and this venue runs against mainnet in practice, so
    // the unchanged-when-unset property guards a real-money account. UNCLAMPED: the
    // venue publishes no ceiling on `exchangeInfo`, so a too-high request is refused by the VENUE
    // (this function's doc).
    let leverage = vike_bridge_core::leverage::leverage_for(req.risk_profile);
    AdapterOutcome {
        exec: VenueAdapterExecutionOutcome::Running(VenueAdapterExecutionRunning {
            client: spawn(ExecutionSpawn {
                urls,
                creds: c,
                symbol: req.symbol.to_string(),
                events: req.events.clone(),
                properties_rec: req.properties_rec,
                builder,
                leverage,
                admitted,
                clock,
                halt_path: req.inputs.process.halt_path.clone(),
            }),
            bound_tier: env,
            grid,
            contract_value: None,
            margin_mode: None,
            leg_grids: Vec::new(),
        }),
        reconcile: recon,
        identity: None,
    }
}

/// aster's start. `vike_tradehub::registry::REGISTRY` holds `&VenueAdapterAster`.
pub struct VenueAdapterAster;

impl VenueAdapter for VenueAdapterAster {
    fn venue(&self) -> &'static str {
        VENUE
    }

    fn declaration(&self) -> VenueAdapterDeclaration {
        VenueAdapterDeclaration {
            addresses_accounts: true,
            process_exclusive: None,
            // Interval-only reconcile: `vike_tradehub_core::assemble_runtime` threads no trigger for this venue.
            // `crates/bridges/aster/src/execution/mod.rs`'s `fetch_aster_properties`: a symbol-scoped
            // blocking pre-fetch.
            grid_source: GridSource::PerSymbolFetch,
            // `crates/bridges/aster/src/transport/sign.rs`'s `load_aster_credentials`: `USER` is the
            // master address (it goes on the wire as `user=`), `PRIVATE_KEY` is the agent signer
            // and `SIGNER` its optional explicit address. So the BOOK is the account's `USER`
            // field, always present (the loader gates on it), and no EVM fallback is wanted —
            // deriving from the agent key would answer with the SIGNER's address, which is not
            // the book.
            book_identity: BookIdentityRule::Named {
                name_fields: &[crate::transport::sign::USER],
                evm_key_fields: &[],
            },
            clock: ClockDeclaration::Wired {
                endpoint: "GET /fapi/v1/time (public)",
                auth: ClockAuthentication::Public,
                risk: ClockRisk::SignedTimestamp,
            },
        }
    }

    /// The network IS the tier: the account's pair arms its own tier (`bound_tier`, which answers
    /// `Live` or `Demo` and never `Paper`) with nothing held below live — a `demo` account's tier is
    /// demo, and a `live` one's is live. Without the pair the venue is paper, and under a `live`
    /// tier the cause is the missing LIVE key set: an account holds one set, so there is no second
    /// (testnet) set to fall back to. The shared one-key-set arm (`resolve_one_key_set`).
    fn resolve(&self, inputs: &AdapterInputs<'_>) -> ArmingResolution {
        resolve_one_key_set(resolved(inputs).is_some(), VenueNetwork::for_tier(inputs.tier))
    }

    fn start(&self, req: AdapterRequest<'_>) -> AdapterOutcome {
        start_with(
            req,
            |urls, c, symbol| crate::fetch_aster_properties(urls, c, symbol).ok().map(|(f, _)| f),
            |s| {
                Box::new(
                    crate::execution::spawn_with_recorder(
                        s.urls,
                        s.creds,
                        s.symbol,
                        s.events,
                        s.properties_rec,
                        s.builder,
                        s.leverage,
                        s.admitted,
                        s.clock,
                    )
                    .with_halt_path(s.halt_path),
                )
            },
        )
    }

    /// aster `/fapi/v1/time` on the FUTURES host the exec client binds, tier-resolved by the SAME
    /// `resolved` answer [`VenueAdapterAster::start`] acts on — there is no `{VENUE}_MAINNET`
    /// switch for this venue, its network IS the account's tier, and testnet when the account
    /// holds no pair (`clock_env`), exactly as `vike-tradehub-core`'s `aster_time` did. The host is
    /// the account's `trade` `usdm` row at that tier (the one [`AsterUrls::exec`] reads); a table
    /// that cannot answer is the read's error, naming the row.
    fn server_time_ms(&self, inputs: &AdapterInputs<'_>, timeout: Duration) -> Result<i64, String> {
        let fapi_rest = inputs
            .settings
            .urls()
            .url(
                UrlType::of_tier(clock_env(inputs)),
                UrlChannel::Trade,
                UrlProtocol::Rest,
                PERP_PRODUCT,
            )
            .map_err(|refusal| refusal.to_string())?;
        parse_server_time(&bounded_public_get(
            VENUE,
            fapi_rest,
            crate::execution::rest::perp::PATH_TIME,
            timeout,
        )?)
    }
}

#[path = "adapter_tests.rs"]
#[cfg(test)]
mod adapter_tests;
