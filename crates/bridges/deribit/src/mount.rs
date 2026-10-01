//! deribit's VENUE MOUNT — the `vike_bridge_core::venue_mount::VenueMount` contract over the
//! testnet exec client and its dedicated authed order-WS reconcile client
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md).
//!
//! Moved, behaviour unchanged, from `vike-mount`: the `("deribit", Some(c))` arm, its row of the
//! legacy arming probe, its clock row WITH the read and parse (`deribit_time`,
//! `parse_deribit_time`), its book-identity row, its grid-source row and its fallback grid.
//!
//! Credentials are the generic `DERIBIT_DEMO_*` pair, always at the DEMO tier: deribit has no
//! `{VENUE}_MAINNET`-shaped switch, its network is not the ceiling's (decision 0095 hands that to
//! binance/bybit/okx and hyperliquid alone), and every exec spawn site binds
//! `crate::transport::TESTNET_REST`.

use std::time::Duration;

use vike_bridge_core::Credentials;
use vike_bridge_core::credentials::{Environment, load_credentials_for_account};
use vike_bridge_core::venue_mount::{
    BookIdentity, ClockAuth, ClockDecl, ClockRisk, DeclaredGridSource, ExecOutcome, HeldBelowLive,
    LiveExec, MountInputs, MountOutcome, MountRequest, PaperCause, Resolution, Tier,
    VenueDeclaration, VenueMount, bounded_public_get, missing_time_field, recon_if_enabled,
};
use vike_model::SymbolProperties;

use crate::data::VENUE;

/// FALLBACK Deribit BTC-PERPETUAL grid, used ONLY if the adapter's startup
/// `public/get_instrument` fetch fails. Amounts are USD contracts on the coin-margined perp
/// (`$10` step, `$0.50` index tick); inert once the real grid loads via
/// `fetch_deribit_properties`. No per-instrument max/min-notional (the option-chain parser's
/// convention — `0.0` is treated as absent by the RiskGate). Moved from `vike-mount`'s
/// `fallback.rs` (`deribit_fallback_properties`).
fn fallback_properties() -> SymbolProperties {
    SymbolProperties {
        tick_size: 0.5,
        step_size: 10.0,
        min_qty: 10.0,
        max_qty: 0.0,
        min_notional: 0.0,
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

/// deribit's JSON-RPC envelope. ⚠ The stamp is a BARE top-level `result` i64 in epoch MS,
/// sitting beside `usIn`/`usOut`/`usDiff` in MICROSECONDS — the nearest wrong field is a
/// thousand-fold out. The `"testnet"` boolean is a free correctness assert no other venue offers:
/// it proves the host that ANSWERED is the tier we bound.
fn parse_server_time(body: &serde_json::Value) -> Result<i64, String> {
    if body.get("testnet").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err(
            "the deribit host answered testnet=false, but every exec spawn site binds testnet — \
             this reading is against the wrong host"
                .to_string(),
        );
    }
    body.get("result")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| missing_time_field("result"))
}

/// deribit's mount. `vike_tradehub::registry::REGISTRY` holds `&DeribitVenueMount`.
pub struct DeribitVenueMount;

impl DeribitVenueMount {
    /// The one gate `resolve` and `mount` share: this account's DEMO key pair.
    fn credentials(inputs: &MountInputs<'_>) -> Option<Credentials> {
        load_credentials_for_account(VENUE, Environment::Demo, inputs.account, inputs.secrets)
    }
}

impl VenueMount for DeribitVenueMount {
    fn venue(&self) -> &'static str {
        VENUE
    }

    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration {
            addresses_accounts: true,
            process_exclusive: None,
            takes_recon_trigger: false,
            // `crate::exec`'s `fetch_deribit_properties`: a symbol-scoped blocking pre-fetch.
            grid_source: DeclaredGridSource::PerSymbolFetch,
            book_identity: BookIdentity::Undeterminable {
                why: "the store holds a client id/secret pair; deribit's subaccount is selected \
                      by the credential and named nowhere in the store",
            },
            // Deribit's own auth carries no timestamp — see `crate::transport`'s `PATH_TIME`.
            // Wired anyway: it is keyless, it answers in ~50 ms from the CI box, and it is the only
            // venue that tells us WHICH HOST answered.
            clock: ClockDecl::Wired {
                endpoint: "GET /api/v2/public/get_time (public, testnet host)",
                auth: ClockAuth::Public,
                risk: ClockRisk::NoTimestamp,
            },
        }
    }

    fn resolve(&self, inputs: &MountInputs<'_>) -> Resolution {
        if Self::credentials(inputs).is_some() {
            Resolution::Armed {
                tier: Tier::Demo,
                held_below_live: Some(HeldBelowLive::DemoOnlyArm),
            }
        } else {
            Resolution::Paper(PaperCause::NoCredentials)
        }
    }

    fn mount(&self, req: MountRequest<'_>) -> MountOutcome {
        let Some(c) = Self::credentials(&req.inputs) else { return MountOutcome::paper() };
        tracing::warn!("deribit: DEMO credentials present → LIVE exec client (real demo orders)");
        // PIT filter recording (opt-in): `req.properties_rec` is the caller-built recorder handle —
        // `None` unless the binary armed one, so the disabled path is byte-identical. It moves into
        // the exec spawn below.
        //
        // Live RiskGate from the venue's REAL grid (PR-2b): one blocking pre-fetch (keyless
        // `public/get_instrument`, a duplicate of the adapter's own in-thread fetch — acceptable,
        // startup-only). On failure `grid` is `None` and the permissive default stands
        // (byte-identical to the pre-live behavior). Deribit is THE motivating venue for the
        // contract size: options/futures are quoted per CONTRACT, so `contract_size` (a real
        // `public/get_instrument` field) is what turns a size into a notional — the one arm that
        // populates a non-1.0 multiplier.
        let grid = crate::exec::fetch_deribit_properties(&c, req.symbol);
        let contract_size = grid.map(|f| f.contract_size);
        // Reconcile handle (audit A1 item 4): `crate::recon_client`'s `recon_client` opens its OWN
        // dedicated authed order-WS (a blocking JSON-RPC auth), so reconcile report fetches never
        // contend the exec side's order-transport `Mutex` — an order submit is never delayed behind
        // a reconcile round-trip. Built from `&c` before `c` moves into the spawn. `None`
        // (auth/connect fail) stays reconcile-inert, exec unaffected.
        //
        // LAZY (`req.recon_enabled`): that blocking authed handshake is skipped entirely when
        // reconciliation is off — the factory is not called, so such a mount does no authenticated
        // network work here.
        let recon = recon_if_enabled(req.recon_enabled, || {
            crate::recon_client::recon_client(&c, req.symbol)
        });
        MountOutcome {
            exec: ExecOutcome::Live(LiveExec {
                client: Box::new(
                    crate::exec::DeribitExecutionClient::spawn_with_recorder(
                        c,
                        req.symbol.to_string(),
                        fallback_properties(),
                        req.events.clone(),
                        req.properties_rec,
                    )
                    .with_halt_path(req.inputs.process.halt_path.clone()),
                ),
                bound_tier: Tier::Demo,
                grid,
                contract_size,
                margin_mode: None,
                leg_grids: Vec::new(),
            }),
            recon,
            identity: None,
        }
    }

    /// `/api/v2/public/get_time` on the TESTNET host — unconditionally, because every exec spawn
    /// site binds `TESTNET_REST` and deribit has no mainnet switch. A check pointed at
    /// `www.deribit.com` would measure a host this mount never talks to.
    fn server_time_ms(&self, _inputs: &MountInputs<'_>, timeout: Duration) -> Result<i64, String> {
        parse_server_time(&bounded_public_get(
            VENUE,
            crate::transport::TESTNET_REST,
            crate::transport::PATH_TIME,
            timeout,
        )?)
    }
}

#[path = "mount_tests.rs"]
#[cfg(test)]
mod mount_tests;
