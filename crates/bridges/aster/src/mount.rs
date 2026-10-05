//! aster's VENUE MOUNT — the `vike_bridge_core::venue_mount::VenueMount` contract over the
//! agent-wallet (EIP-712) exec client, its reconcile client and its public server clock
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md).
//!
//! Moved from `vike-mount`: its `("aster", _)` arm (now [`AsterVenueMount::mount`]), its
//! arming-probe row ([`AsterVenueMount::resolve`]), its clock row WITH the read (`vike-mount`'s
//! `aster_time`, now [`AsterVenueMount::server_time_ms`]), its book-identity and grid-source rows
//! and its fallback grid.
//!
//! Aster uses agent-wallet creds (`ASTER_{LIVE|TESTNET}_{USER,PRIVATE_KEY,SIGNER}`), NOT the
//! generic `{VENUE}_DEMO_*` shape. It is SWITCHLESS — there is no `{VENUE}_MAINNET` switch for it;
//! its tier is which key set resolves (`crate::signing`'s `mountable_tier_for_account`): LIVE
//! (mainnet) first, but only when the ceiling permits it, else TESTNET. ⚠ REAL MONEY:
//! `ASTER_LIVE_*` present under a `live` ceiling spawns a LIVE MAINNET exec client, and this venue
//! runs against mainnet in practice because no testnet credentials are configured
//! (`crates/bridges/aster/CLAUDE.md`'s correction record).

use std::sync::Arc;
use std::time::Duration;

use vike_bridge_core::credentials::{Credentials, Environment, attribution_code_from};
use vike_bridge_core::venue_mount::{
    BookIdentity, ClockAuth, ClockDecl, ClockRisk, DeclaredGridSource, ExecOutcome, HeldBelowLive,
    LiveExec, MountInputs, MountOutcome, MountRequest, PaperCause, Resolution, Tier,
    VenueDeclaration, VenueMount, bounded_public_get, missing_time_field,
};
use vike_exec::{EventSender, ExecutionClient};
use vike_model::SymbolProperties;
use vike_model::accounts::account_keys::AccountLabel;

use crate::signing::mountable_tier_for_account;
use crate::urls::{VENUE, urls_for};

/// FALLBACK Aster BTCUSDT grid, used ONLY if the Aster adapter's startup `exchangeInfo` fetch
/// fails. Aster is a Binance USDⓈ-M/spot fork, so the grid mirrors binance's fallback grid
/// (identical values); inert once the real grid loads via `fetch_aster_properties`. Moved from
/// `vike-mount`'s `fallback.rs`.
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
/// name is gated by a fixture test rather than by a live call (was `vike-mount`'s
/// `parse_binance_shaped_time`, deleted when binance's port moved that venue's read into its own
/// bridge).
fn parse_server_time(body: &serde_json::Value) -> Result<i64, String> {
    body.get("serverTime")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| missing_time_field("serverTime"))
}

/// The key set this account's mount resolves, and the network it binds — the ONE answer
/// [`AsterVenueMount::resolve`] reports and [`AsterVenueMount::mount`] acts on.
fn resolved(inputs: &MountInputs<'_>) -> Option<(Environment, Credentials)> {
    mountable_tier_for_account(inputs.account, inputs.secrets, inputs.live_permitted)
}

/// The network whose clock [`AsterVenueMount::server_time_ms`] reads: the one the DEFAULT
/// account's mount would bind under this ceiling, through the same `mountable_tier_for_account`
/// chain, and testnet when nothing resolves — exactly what `vike-mount`'s `aster_time` read. The
/// ceiling is load-bearing: this is a `ClockRisk::SignedTimestamp` row, whose clock FAIL demotes
/// the venue to paper (`vike-mount`'s preflight `venue_disposition`), so a demo-capped box holding
/// both key sets must be judged by the testnet host it signs for, never by mainnet's.
///
/// ⚠ DEFAULT account on purpose, and so not [`resolved`]: the mount binds the NAMED account's key
/// set, while the clock is read once per venue.
fn clock_env(inputs: &MountInputs<'_>) -> Environment {
    mountable_tier_for_account(&AccountLabel::Default, inputs.secrets, inputs.live_permitted)
        .map(|(env, _)| env)
        .unwrap_or(Environment::Demo)
}

/// The tier a resolved network authenticates — the ONE mapping [`AsterVenueMount::resolve`]
/// reports and [`AsterVenueMount::mount`] binds, so the arming screen and the identity record
/// cannot name different tiers.
///
/// ⚠ THE BOUND-TIER FIX — the venue mount contract spec's Finding 1, and the migration's ONE
/// intended behaviour change. `vike-mount`'s legacy fold addressed this venue's identity record at
/// its CEX conjunct `ceiling_selects_mainnet(venue) && live_permitted`, which is `false` for
/// aster, so a LIVE aster mount addressed the DEMO row. It was harmless only because
/// `crate::recon_client`'s `AsterReconClient` does not forward `fetch_account_identity`, so nothing
/// was ever recorded; had a one-line forward there existed, a live identity would have been
/// recorded against the demo row.
fn bound_tier(env: Environment) -> Tier {
    match env {
        Environment::Live => Tier::Live,
        _ => Tier::Demo,
    }
}

/// `crate::AsterExecutionClient::spawn_with_recorder`'s arguments, in its order, as [`mount_with`]
/// assembles them — the one value its spawn step is handed.
struct ExecSpawn {
    env: Environment,
    creds: Credentials,
    symbol: String,
    fallback: SymbolProperties,
    events: EventSender,
    properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
    builder: Option<(String, String)>,
    leverage: f64,
    /// The HALT sentinel the client watches — `MountInputs::process.halt_path`, handed to the
    /// client by its `with_halt_path` (decision 0099). Not an argument of `spawn_with_recorder`:
    /// it rides the builder, applied by `mount`'s spawn step.
    halt_path: std::path::PathBuf,
}

/// [`AsterVenueMount::mount`]'s body, with its two NETWORK steps — the grid pre-fetch, and the exec
/// spawn whose thread dials the venue — taken as parameters. `mount` passes the real two; a test
/// passes offline doubles, which is what lets `mount_tests.rs`'s bound-tier pin read the tier THIS
/// body binds rather than what a helper beside it answers. Nothing else here reaches a socket:
/// `crate::recon_client` only constructs.
fn mount_with(
    req: MountRequest<'_>,
    fetch_grid: impl FnOnce(Environment, &Credentials, &str) -> Option<SymbolProperties>,
    spawn: impl FnOnce(ExecSpawn) -> Box<dyn ExecutionClient + Send>,
) -> MountOutcome {
    let Some((env, c)) = resolved(&req.inputs) else { return MountOutcome::paper() };
    // Two lines, not one with a substituted word: the convention
    // (`crates/vike-ops/tests/live_mount_line_gate.rs`) puts `⚠ REAL-MONEY: ` on a live tier and
    // on nothing else, and a tier that varies at run time cannot be spelled `tier = …` as a literal.
    if env == Environment::Live {
        tracing::warn!(
            venue = VENUE,
            account = %req.inputs.account,
            tier = Tier::Live.as_str(),
            "⚠ REAL-MONEY: aster: LIVE (mainnet) credentials present → LIVE exec client (REAL MONEY)"
        );
    } else {
        tracing::warn!(
            venue = VENUE,
            account = %req.inputs.account,
            tier = Tier::Demo.as_str(),
            "aster: TESTNET credentials present → LIVE exec client (testnet orders)"
        );
    }
    // Live RiskGate from the venue's REAL grid (PR-2b): one blocking PUBLIC `exchangeInfo`
    // pre-fetch (spot or USDⓈ-M perp, routed by the `.P` suffix). It DUPLICATES the exec adapter's
    // own in-thread fetch of the same grid, so two `exchangeInfo` requests at startup are expected
    // (acceptable, startup-only). On failure the permissive default stands.
    let grid = fetch_grid(env, &c, req.symbol);
    // Reconcile handle from the RESOLVED env + agent creds (µs EIP-712 signer), so it hits the
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
    // ACCOUNT LEVERAGE from the operator's `[risk]` budget; unset ⇒ the historical 2.0. ⚠ This
    // venue tries its LIVE creds first under a `live` ceiling and runs against mainnet in practice,
    // so the unchanged-when-unset property guards a real-money account. UNCLAMPED: the venue
    // publishes no ceiling on `exchangeInfo`, so a too-high request is refused by the VENUE
    // (`leverage_for`'s doc).
    let leverage = crate::exec::leverage_for(req.risk_profile);
    MountOutcome {
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
            bound_tier: bound_tier(env),
            grid,
            contract_size: None,
            margin_mode: None,
            leg_grids: Vec::new(),
        }),
        recon,
        identity: None,
    }
}

/// aster's mount. `vike_tradehub::registry::REGISTRY` holds `&AsterVenueMount`.
pub struct AsterVenueMount;

impl VenueMount for AsterVenueMount {
    fn venue(&self) -> &'static str {
        VENUE
    }

    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration {
            addresses_accounts: true,
            process_exclusive: None,
            // Interval-only reconcile: `vike_mount::build_node` threads no trigger for this venue.
            takes_recon_trigger: false,
            // `crates/bridges/aster/src/exec.rs`'s `fetch_aster_properties`: a symbol-scoped
            // blocking pre-fetch.
            grid_source: DeclaredGridSource::PerSymbolFetch,
            // `crates/bridges/aster/src/signing.rs`'s `load_aster_credentials_for_account`: `USER`
            // is the master address (it goes on the wire as `user=`), `PRIVATE_KEY` is the agent
            // signer and `SIGNER` its optional explicit address. So the BOOK is `USER`, always
            // present (the loader gates on it), and no EVM fallback is wanted — deriving from the
            // agent key would answer with the SIGNER's address, which is not the book.
            book_identity: BookIdentity::Named {
                prefix: "ASTER",
                demo_tiers: &["TESTNET"],
                live_tiers: &["LIVE"],
                name_suffixes: &["USER"],
                evm_key_suffixes: &[],
            },
            clock: ClockDecl::Wired {
                endpoint: "GET /fapi/v1/time (public)",
                auth: ClockAuth::Public,
                risk: ClockRisk::SignedTimestamp,
            },
        }
    }

    fn resolve(&self, inputs: &MountInputs<'_>) -> Resolution {
        match resolved(inputs).map(|(env, _)| bound_tier(env)) {
            Some(Tier::Live) => Resolution::Armed { tier: Tier::Live, held_below_live: None },
            // Switchless: what selects the tier is WHICH key set exists, so a live ceiling with
            // only testnet keys is a credential fact, not a flag fact.
            Some(Tier::Demo) => Resolution::Armed {
                tier: Tier::Demo,
                held_below_live: Some(HeldBelowLive::LiveCredentialsAbsent),
            },
            None => Resolution::Paper(PaperCause::NoCredentials),
        }
    }

    fn mount(&self, req: MountRequest<'_>) -> MountOutcome {
        mount_with(
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
    /// `mountable_tier_for_account` chain [`AsterVenueMount::mount`] calls, under the same ceiling
    /// — there is no `{VENUE}_MAINNET` switch for this venue, its tier IS which key set resolves.
    /// It reads the DEFAULT account's keys (`clock_env`), exactly as `vike-mount`'s `aster_time`
    /// did.
    fn server_time_ms(&self, inputs: &MountInputs<'_>, timeout: Duration) -> Result<i64, String> {
        parse_server_time(&bounded_public_get(
            VENUE,
            urls_for(clock_env(inputs)).fapi_rest,
            crate::perp::PATH_TIME,
            timeout,
        )?)
    }
}

#[path = "mount_tests.rs"]
#[cfg(test)]
mod mount_tests;
