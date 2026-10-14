//! aster's VENUE ADAPTER — the `vike_bridge_core::venue_adapter::VenueAdapter` contract over the
//! agent-wallet (EIP-712) exec client, its reconcile client and its public server clock
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md).
//!
//! Moved from `vike-tradehub-core`: its `("aster", _)` arm (now [`AsterVenueAdapter::start`]), its
//! arming-probe row ([`AsterVenueAdapter::resolve`]), its clock row WITH the read (`vike-tradehub-core`'s
//! `aster_time`, now [`AsterVenueAdapter::server_time_ms`]), its book-identity and grid-source rows
//! and its fallback grid.
//!
//! Aster uses agent-wallet FIELDS of the started account (`USER`, `PRIVATE_KEY`, optional
//! `SIGNER`: `crate::signing`'s `load_aster_credentials`), NOT the generic `API_KEY`/`API_SECRET`
//! pair. It is SWITCHLESS — there is no `{VENUE}_MAINNET` switch for it; the network IS the
//! account's tier (`AdapterInputs::tier`): a `live` account signs on mainnet, a `demo` one on
//! testnet. ⚠ REAL MONEY: a `live`-tier account holding its pair spawns a LIVE MAINNET exec client,
//! and this venue runs against mainnet in practice because no testnet credentials are configured
//! (`crates/bridges/aster/CLAUDE.md`'s correction record). A `live`-tier account missing either
//! field does NOT RUN (`PaperCause::LiveCredentialsAbsent`): an account holds ONE key set, so there
//! is no second (testnet) set beside it to fall back to (decision 0156 item 6).

use std::sync::Arc;
use std::time::Duration;

use vike_bridge_core::venue_adapter::{
    AdapterInputs, AdapterOutcome, AdapterRequest, BookIdentity, ClockAuth, ClockDecl, ClockRisk,
    DeclaredGridSource, ExecOutcome, LiveExec, PaperCause, Resolution, VenueAdapter,
    VenueDeclaration, bounded_public_get, missing_time_field,
};
use vike_exec::{EventSender, ExecutionClient};
use vike_model::{AccountTier, SymbolProperties};
use vike_secrets::credentials::{Credentials, attribution_code_from};

use crate::signing::load_aster_credentials;
use crate::urls::{VENUE, urls_for};

/// FALLBACK Aster BTCUSDT grid, used ONLY if the Aster adapter's startup `exchangeInfo` fetch
/// fails. Aster is a Binance USDⓈ-M/spot fork, so the grid mirrors binance's fallback grid
/// (identical values); inert once the real grid loads via `fetch_aster_properties`. Moved from
/// `vike-tradehub-core`'s `fallback.rs`.
fn fallback_properties() -> SymbolProperties {
    SymbolProperties {
        tick_size: 0.01,
        step_size: 0.00001,
        min_qty: 0.00001,
        max_qty: 100_000.0,
        min_notional: 5.0,
        // Every remaining field stays ABSENT, via functional-update syntax: no contract size
        // (= multiplier 1.0), no tiered grid (the scalar tick above IS the whole grid), no venue
        // hold. A fallback fires only when the venue fetch FAILED, and inventing an unverified
        // value there would silently change the degraded path's behavior — absent is today's.
        // `..Default::default()` rather than an exhaustive literal ON PURPOSE: `SymbolProperties`
        // grows (contract_size, tick_scheme, taker_hold_ms all cost every construction site in the
        // workspace an edit), and a fallback grid must never have to be touched for a field it
        // does not know about.
        ..Default::default()
    }
}

/// `{"serverTime": <epoch ms>}` — aster is binance-API-shaped. Split from the fetch so the field
/// name is gated by a fixture test rather than by a live call (was `vike-tradehub-core`'s
/// `parse_binance_shaped_time`, deleted when binance's port moved that venue's read into its own
/// bridge).
fn parse_server_time(body: &serde_json::Value) -> Result<i64, String> {
    body.get("serverTime")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| missing_time_field("serverTime"))
}

/// The account's key set and the network it binds — the ONE answer [`AsterVenueAdapter::resolve`]
/// reports and [`AsterVenueAdapter::start`] acts on. The network is [`bound_tier`] of the account's
/// tier; the key set is the account's own pair (`None` = the live gate).
fn resolved(inputs: &AdapterInputs<'_>) -> Option<(AccountTier, Credentials)> {
    load_aster_credentials(&inputs.credentials).map(|c| (bound_tier(inputs.tier), c))
}

/// The network whose clock [`AsterVenueAdapter::server_time_ms`] reads: the one [`resolved`] binds,
/// and testnet when nothing resolves — exactly what `vike-tradehub-core`'s `aster_time` read. The tier is
/// load-bearing: this is a `ClockRisk::SignedTimestamp` row, whose clock FAIL demotes the venue to
/// paper (`vike-tradehub-core`'s preflight `venue_disposition`), so a demo-tier account must be judged by
/// the testnet host it signs for, never by mainnet's.
///
/// `vike-tradehub-core` reads the clock once per venue, on the DEFAULT account's inputs
/// (`crates/vike-tradehub-core/src/server_time.rs`'s `venue_server_time_ms_under_policy`), so the key set
/// consulted here is that account's.
fn clock_env(inputs: &AdapterInputs<'_>) -> AccountTier {
    resolved(inputs).map_or(AccountTier::Demo, |(env, _)| env)
}

/// The network (and so the tier) an account of `tier` authenticates — the ONE mapping
/// [`AsterVenueAdapter::resolve`] reports and [`AsterVenueAdapter::start`] binds, so the arming screen
/// and the identity record cannot name different tiers. `Live` → mainnet; `Demo` (and `Paper`, read
/// as `Demo`) → testnet.
///
/// ⚠ THE BOUND-TIER FIX — the venue-adapter contract spec's Finding 1, and the migration's ONE
/// intended behaviour change. `vike-tradehub-core`'s legacy fold addressed this venue's identity record at
/// its CEX conjunct `ceiling_selects_mainnet(venue) && live_permitted`, which is `false` for
/// aster, so a LIVE aster start addressed the DEMO row. It was harmless only because
/// `crate::recon_client`'s `AsterReconClient` does not forward `fetch_account_identity`, so nothing
/// was ever recorded; had a one-line forward there existed, a live identity would have been
/// recorded against the demo row.
fn bound_tier(tier: AccountTier) -> AccountTier {
    match tier {
        AccountTier::Live => AccountTier::Live,
        AccountTier::Demo | AccountTier::Paper => AccountTier::Demo,
    }
}

/// `crate::AsterExecutionClient::spawn_with_recorder`'s arguments, in its order, as [`start_with`]
/// assembles them — the one value its spawn step is handed.
struct ExecSpawn {
    env: AccountTier,
    creds: Credentials,
    symbol: String,
    fallback: SymbolProperties,
    events: EventSender,
    properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
    builder: Option<(String, String)>,
    leverage: f64,
    /// The HALT sentinel the client watches — `AdapterInputs::process.halt_path`, handed to the
    /// client by its `with_halt_path` (decision 0099). Not an argument of `spawn_with_recorder`:
    /// it rides the builder, applied by `start`'s spawn step.
    halt_path: std::path::PathBuf,
}

/// [`AsterVenueAdapter::start`]'s body, with its two NETWORK steps — the grid pre-fetch, and the exec
/// spawn whose thread dials the venue — taken as parameters. `start` passes the real two; a test
/// passes offline doubles, which is what lets `adapter_tests.rs`'s bound-tier pin read the tier THIS
/// body binds rather than what a helper beside it answers. Nothing else here reaches a socket:
/// `crate::recon_client` only constructs.
fn start_with(
    req: AdapterRequest<'_>,
    fetch_grid: impl FnOnce(AccountTier, &Credentials, &str) -> Option<SymbolProperties>,
    spawn: impl FnOnce(ExecSpawn) -> Box<dyn ExecutionClient + Send>,
) -> AdapterOutcome {
    let Some((env, c)) = resolved(&req.inputs) else { return AdapterOutcome::paper() };
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
    let grid = fetch_grid(env, &c, req.symbol);
    // Reconcile handle from the account's network + agent creds (µs EIP-712 signer), so it hits the
    // SAME network the exec client does. Pure to construct, so NOT gated on `recon_enabled` —
    // exactly as the arm was. Borrows `&c` before it moves into the spawn.
    let recon = crate::recon_client(env, &c, req.symbol);
    // Aster Code (unified cross-venue attribution): `attribution_code_from` validates the address
    // against `aster`'s `AttributionMechanic::SignedBuilder`; absent/invalid degrades to `None` —
    // no `builder`/`feeRate` on the wire at all. The fee defaults to `"0"` (attribution-only, no
    // `approveBuilder` grant needed — see `crate::perp::AsterPerpRest::approve_builder`'s doc)
    // unless `ASTER_BUILDER_FEE_RATE` is set AND the operator has separately run the one-time
    // approval.
    let builder = attribution_code_from(req.inputs.secrets, VENUE).map(|address| {
        let fee_rate = req
            .inputs
            .secrets
            .get("ASTER_BUILDER_FEE_RATE")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "0".to_string());
        (address, fee_rate)
    });
    // ACCOUNT LEVERAGE from the operator's `[risk]` budget; unset ⇒ the historical 2.0. ⚠ A
    // `live`-tier account signs on mainnet and this venue runs against mainnet in practice, so
    // the unchanged-when-unset property guards a real-money account. UNCLAMPED: the
    // venue publishes no ceiling on `exchangeInfo`, so a too-high request is refused by the VENUE
    // (`leverage_for`'s doc).
    let leverage = crate::exec::leverage_for(req.risk_profile);
    AdapterOutcome {
        exec: ExecOutcome::Live(LiveExec {
            client: spawn(ExecSpawn {
                env,
                creds: c,
                symbol: req.symbol.to_string(),
                fallback: fallback_properties(),
                events: req.events.clone(),
                properties_rec: req.properties_rec,
                builder,
                leverage,
                halt_path: req.inputs.process.halt_path.clone(),
            }),
            bound_tier: env,
            grid,
            contract_size: None,
            margin_mode: None,
            leg_grids: Vec::new(),
        }),
        recon,
        identity: None,
    }
}

/// aster's start. `vike_tradehub::registry::REGISTRY` holds `&AsterVenueAdapter`.
pub struct AsterVenueAdapter;

impl VenueAdapter for AsterVenueAdapter {
    fn venue(&self) -> &'static str {
        VENUE
    }

    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration {
            addresses_accounts: true,
            process_exclusive: None,
            // Interval-only reconcile: `vike_tradehub_core::assemble_runtime` threads no trigger for this venue.
            takes_recon_trigger: false,
            // `crates/bridges/aster/src/exec.rs`'s `fetch_aster_properties`: a symbol-scoped
            // blocking pre-fetch.
            grid_source: DeclaredGridSource::PerSymbolFetch,
            // `crates/bridges/aster/src/signing.rs`'s `load_aster_credentials`: `USER` is the
            // master address (it goes on the wire as `user=`), `PRIVATE_KEY` is the agent signer
            // and `SIGNER` its optional explicit address. So the BOOK is the account's `USER`
            // field, always present (the loader gates on it), and no EVM fallback is wanted —
            // deriving from the agent key would answer with the SIGNER's address, which is not
            // the book.
            book_identity: BookIdentity::Named {
                name_fields: &[crate::signing::USER],
                evm_key_fields: &[],
            },
            clock: ClockDecl::Wired {
                endpoint: "GET /fapi/v1/time (public)",
                auth: ClockAuth::Public,
                risk: ClockRisk::SignedTimestamp,
            },
        }
    }

    /// The network IS the tier: the account's pair arms its own tier (`bound_tier`, which answers
    /// `Live` or `Demo` and never `Paper`) with nothing held below live — a `demo` account's tier is
    /// demo, and a `live` one's is live. Without the pair the venue is paper, and under a `live`
    /// tier the cause is the missing LIVE key set: an account holds one set, so there is no second
    /// (testnet) set to fall back to.
    fn resolve(&self, inputs: &AdapterInputs<'_>) -> Resolution {
        match (resolved(inputs), inputs.tier) {
            (Some((env, _)), _) => Resolution::Armed { tier: env, held_below_live: None },
            (None, AccountTier::Live) => Resolution::Paper(PaperCause::LiveCredentialsAbsent),
            (None, AccountTier::Demo | AccountTier::Paper) => {
                Resolution::Paper(PaperCause::NoCredentials)
            }
        }
    }

    fn start(&self, req: AdapterRequest<'_>) -> AdapterOutcome {
        start_with(
            req,
            |env, c, symbol| crate::fetch_aster_properties(env, c, symbol).map(|(f, _base)| f),
            |s| {
                Box::new(
                    crate::AsterExecutionClient::spawn_with_recorder(
                        s.env,
                        s.creds,
                        s.symbol,
                        s.fallback,
                        s.events,
                        s.properties_rec,
                        s.builder,
                        s.leverage,
                    )
                    .with_halt_path(s.halt_path),
                )
            },
        )
    }

    /// aster `/fapi/v1/time` on the FUTURES host the exec client binds, tier-resolved by the SAME
    /// `resolved` answer [`AsterVenueAdapter::start`] acts on — there is no `{VENUE}_MAINNET`
    /// switch for this venue, its network IS the account's tier, and testnet when the account
    /// holds no pair (`clock_env`), exactly as `vike-tradehub-core`'s `aster_time` did.
    fn server_time_ms(&self, inputs: &AdapterInputs<'_>, timeout: Duration) -> Result<i64, String> {
        parse_server_time(&bounded_public_get(
            VENUE,
            urls_for(clock_env(inputs)).fapi_rest,
            crate::perp::PATH_TIME,
            timeout,
        )?)
    }
}

#[path = "adapter_tests.rs"]
#[cfg(test)]
mod adapter_tests;
