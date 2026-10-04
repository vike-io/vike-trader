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
//!
//! ⚠ **A LIVE-TIER KEY SET IS NAMED, NOT IGNORED.** A store holding a COMPLETE `DERIBIT_LIVE_*` (or
//! legacy `DERIBIT_MAINNET_*`) pair and no demo one used to answer `NoCredentials` in silence — the
//! same words as an empty store. It now answers [`PaperCause::LiveTierNotWired`] and `mount` says so
//! at `error!` (`vike_bridge_core::venue_mount::report_unused_live_tier`). Nothing else moves: a live
//! pair BESIDE a demo one still mounts the testnet, and no venue mounts live that did not before.
//! What `mount` says about the live tier is ONE function over the states the store can be in
//! (`DeribitVenueMount::say_what_the_live_tier_is`): a complete live pair beside the demo one is a
//! `warn!` that it is unused, once per process per account; a complete one alone is the `error!`
//! above; a HALF-written one (a typo'd secret name) is an `error!` naming the key it lacks. The same
//! function speaks for a labelled account that is never mounted
//! ([`VenueMount::report_unmounted_account`]).

use std::sync::Arc;
use std::time::Duration;

use vike_bridge_core::Credentials;
use vike_bridge_core::credentials::{
    Environment, load_credentials_for_account, tier_keys_for_account,
};
use vike_bridge_core::venue_mount::{
    BookIdentity, ClockAuth, ClockDecl, ClockRisk, DeclaredGridSource, ExecOutcome, HeldBelowLive,
    LiveExec, LiveTierSet, MountInputs, MountOutcome, MountRequest, PaperCause, Resolution, Tier,
    VenueDeclaration, VenueMount, bounded_public_get, missing_time_field, recon_if_enabled,
    report_unused_live_tier,
};
use vike_exec::recon::ReconClient;
use vike_exec::{EventSender, ExecutionClient};
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

    /// Whether this account's store holds a COMPLETE LIVE-tier key pair (the legacy `MAINNET` tier
    /// included — the generic loader honours it). This arm never selects it, so its only use is to
    /// name the cause [`PaperCause::LiveTierNotWired`] and to say so at `mount`.
    fn live_tier_present(inputs: &MountInputs<'_>) -> bool {
        load_credentials_for_account(VENUE, Environment::Live, inputs.account, inputs.secrets)
            .is_some()
    }

    /// What the store holds of this account's LIVE tier: complete (the loader's own verdict), a
    /// half-written pair (the names the generic loader reads, current tier and legacy `MAINNET`
    /// alike), or nothing.
    fn live_tier(inputs: &MountInputs<'_>) -> LiveTierSet {
        LiveTierSet::read(
            Self::live_tier_present(inputs),
            &tier_keys_for_account(VENUE, Environment::Live, inputs.account),
            inputs.secrets,
        )
    }

    /// **Everything this arm says about the LIVE tier**, in one place: `mount` calls it for the
    /// account it is mounting and [`VenueMount::report_unmounted_account`] for a labelled account
    /// that is never mounted, so the two cannot word one fact two ways. `demo_mounts` is whether the
    /// demo pair loaded.
    fn say_what_the_live_tier_is(inputs: &MountInputs<'_>, demo_mounts: bool) {
        report_unused_live_tier(
            VENUE,
            inputs.account,
            "testnet",
            demo_mounts,
            &Self::live_tier(inputs),
        );
    }
}

/// [`DeribitVenueMount::mount`]'s body, with its three NETWORK steps taken as parameters:
/// `fetch_grid` is the blocking keyless `public/get_instrument` pre-fetch, `connect_recon` the
/// reconcile client's authed order-WS handshake and `spawn` the exec client. `mount` passes the real
/// three; a test passes offline doubles, which is what lets `mount_tests.rs` run the branch that
/// finds a demo key pair (every other branch of this body is already offline) — the only other way
/// to reach it is to dial the testnet host.
fn mount_with(
    req: MountRequest<'_>,
    fetch_grid: impl FnOnce(&Credentials, &str) -> Option<SymbolProperties>,
    connect_recon: impl FnOnce(&Credentials, &str) -> Option<Box<dyn ReconClient>>,
    spawn: impl FnOnce(
        Credentials,
        String,
        EventSender,
        Option<Arc<vike_data::PropertiesRecorder>>,
    ) -> Box<dyn ExecutionClient + Send>,
) -> MountOutcome {
    // A LIVE-tier pair is the one thing about the store that is said either way: alone (or half
    // written) it is why the venue stays paper, beside the demo pair it is why nothing changed.
    let credentials = DeribitVenueMount::credentials(&req.inputs);
    DeribitVenueMount::say_what_the_live_tier_is(&req.inputs, credentials.is_some());
    let Some(c) = credentials else {
        return MountOutcome::paper();
    };
    tracing::warn!(
        venue = VENUE,
        account = %req.inputs.account,
        tier = Tier::Demo.as_str(),
        "deribit: DEMO credentials present → LIVE exec client (real demo orders)"
    );
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
    let grid = fetch_grid(&c, req.symbol);
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
    let recon = recon_if_enabled(req.recon_enabled, || connect_recon(&c, req.symbol));
    MountOutcome {
        exec: ExecOutcome::Live(LiveExec {
            client: spawn(c, req.symbol.to_string(), req.events.clone(), req.properties_rec),
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
        } else if Self::live_tier_present(inputs) {
            Resolution::Paper(PaperCause::LiveTierNotWired)
        } else {
            Resolution::Paper(PaperCause::NoCredentials)
        }
    }

    fn mount(&self, req: MountRequest<'_>) -> MountOutcome {
        // Read BEFORE `req` moves into `mount_with`; the spawn closure hands it to the client.
        let halt_path = req.inputs.process.halt_path.clone();
        mount_with(
            req,
            crate::exec::fetch_deribit_properties,
            crate::recon_client::recon_client,
            move |creds, symbol, events, properties_rec| {
                Box::new(
                    crate::exec::DeribitExecutionClient::spawn_with_recorder(
                        creds,
                        symbol,
                        fallback_properties(),
                        events,
                        properties_rec,
                    )
                    .with_halt_path(halt_path),
                )
            },
        )
    }

    /// A labelled account is mounted only when it armed, so one with no demo pair never reaches
    /// `mount`; `vike-mount` asks for what `mount` would have said (see the trait method).
    fn report_unmounted_account(&self, inputs: &MountInputs<'_>) {
        Self::say_what_the_live_tier_is(inputs, Self::credentials(inputs).is_some());
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
