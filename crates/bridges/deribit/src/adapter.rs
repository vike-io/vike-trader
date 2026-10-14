//! deribit's VENUE ADAPTER — the `vike_bridge_core::venue_adapter::VenueAdapter` contract over the
//! testnet exec client and its dedicated authed order-WS reconcile client
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md).
//!
//! Moved, behaviour unchanged, from `vike-tradehub-core`: the `("deribit", Some(c))` arm, its row of the
//! legacy arming probe, its clock row WITH the read and parse (`deribit_time`,
//! `parse_deribit_time`), its book-identity row, its grid-source row and its fallback grid.
//!
//! Credentials are the generic pair — the started ACCOUNT's `API_KEY` + `API_SECRET` fields
//! (`AdapterInputs::credentials`, decision 0156 item 6) — and this is a DEMO-ONLY arm: deribit has no
//! `{VENUE}_MAINNET`-shaped switch, its network is not the account tier's (decision 0095 hands that
//! to binance/bybit/okx and hyperliquid alone), and every exec spawn site binds
//! `crate::transport::TESTNET_REST`. So a `demo` account holding its pair arms the testnet, held
//! below live as [`HeldBelowLive::DemoOnlyArm`].
//!
//! ⚠ **A LIVE ACCOUNT'S KEY SET IS NAMED, NOT IGNORED.** An account whose tier is `live` and which
//! holds a complete pair would otherwise read like an empty store. It answers
//! [`PaperCause::LiveTierNotWired`] and `start` says so ONCE at `error!`
//! (`vike_bridge_core::venue_adapter::report_live_tier_not_wired`), then does not run: no venue starts
//! live that did not before. What the arm says about a live account is ONE function
//! (`DeribitVenueAdapter::say_what_the_live_tier_is`), which speaks for a labelled account that is
//! never started too ([`VenueAdapter::report_not_running_account`]). An account with no complete pair
//! is `NoCredentials`, silently, at either tier.

use std::sync::Arc;
use std::time::Duration;

use vike_bridge_core::venue_adapter::{
    AdapterInputs, AdapterOutcome, AdapterRequest, BookIdentity, ClockAuth, ClockDecl, ClockRisk,
    DeclaredGridSource, ExecOutcome, HeldBelowLive, LiveExec, PaperCause, Resolution, VenueAdapter,
    VenueDeclaration, bounded_public_get, missing_time_field, recon_if_enabled,
    report_live_tier_not_wired,
};
use vike_exec::recon::ReconClient;
use vike_exec::{EventSender, ExecutionClient};
use vike_model::{AccountTier, SymbolProperties};
use vike_secrets::credentials::{Credentials, api_credentials};

use crate::data::VENUE;

/// FALLBACK Deribit BTC-PERPETUAL grid, used ONLY if the adapter's startup
/// `public/get_instrument` fetch fails. Amounts are USD contracts on the coin-margined perp
/// (`$10` step, `$0.50` index tick); inert once the real grid loads via
/// `fetch_deribit_properties`. No per-instrument max/min-notional (the option-chain parser's
/// convention — `0.0` is treated as absent by the RiskGate). Moved from `vike-tradehub-core`'s
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

/// deribit's start. `vike_tradehub::registry::REGISTRY` holds `&DeribitVenueAdapter`.
pub struct DeribitVenueAdapter;

impl DeribitVenueAdapter {
    /// The one gate `resolve` and `start` share: THIS account's key pair — its `API_KEY` and
    /// `API_SECRET` fields (`vike_secrets::credentials::api_credentials`).
    fn credentials(inputs: &AdapterInputs<'_>) -> Option<Credentials> {
        api_credentials(&inputs.credentials, VENUE)
    }

    /// **The demo-only arm's answer**, which `resolve` returns and `start` acts on: a complete pair
    /// on a `demo` account (or `paper`, read as demo) arms the testnet, held below live as
    /// [`HeldBelowLive::DemoOnlyArm`]; a complete pair on a `live` account is
    /// [`PaperCause::LiveTierNotWired`] — this arm has no live network to bind; no complete pair is
    /// [`PaperCause::NoCredentials`] at either tier.
    fn resolution(inputs: &AdapterInputs<'_>) -> Resolution {
        if Self::credentials(inputs).is_none() {
            return Resolution::Paper(PaperCause::NoCredentials);
        }
        // ⚠ The live tier is the `else` on purpose: this demo-only arm never NAMES it in production
        // code (`crates/vike-ops/tests/venues/live_mount_line_gate.rs` reads a named live tier as an
        // arm that can bind it).
        if matches!(inputs.tier, AccountTier::Demo | AccountTier::Paper) {
            Resolution::Armed {
                tier: AccountTier::Demo,
                held_below_live: Some(HeldBelowLive::DemoOnlyArm),
            }
        } else {
            Resolution::Paper(PaperCause::LiveTierNotWired)
        }
    }

    /// **Everything this arm says about a LIVE account**, in one place: `start` calls it for the
    /// account it is starting and [`VenueAdapter::report_not_running_account`] for a labelled account
    /// that is never started, so the two cannot word one fact two ways. It speaks exactly when the
    /// answer is [`PaperCause::LiveTierNotWired`], and says nothing otherwise.
    fn say_what_the_live_tier_is(inputs: &AdapterInputs<'_>) {
        if Self::resolution(inputs) == Resolution::Paper(PaperCause::LiveTierNotWired) {
            report_live_tier_not_wired(VENUE, inputs.account, "testnet");
        }
    }
}

/// [`DeribitVenueAdapter::start`]'s body, with its three NETWORK steps taken as parameters:
/// `fetch_grid` is the blocking keyless `public/get_instrument` pre-fetch, `connect_recon` the
/// reconcile client's authed order-WS handshake and `spawn` the exec client. `start` passes the real
/// three; a test passes offline doubles, which is what lets `adapter_tests.rs` run the branch that
/// arms a demo account (every other branch of this body is already offline) — the only other way
/// to reach it is to dial the testnet host.
fn start_with(
    req: AdapterRequest<'_>,
    fetch_grid: impl FnOnce(&Credentials, &str) -> Option<SymbolProperties>,
    connect_recon: impl FnOnce(&Credentials, &str) -> Option<Box<dyn ReconClient>>,
    spawn: impl FnOnce(
        Credentials,
        String,
        EventSender,
        Option<Arc<vike_data::PropertiesRecorder>>,
    ) -> Box<dyn ExecutionClient + Send>,
) -> AdapterOutcome {
    // A LIVE account holding its pair is said out loud — it is why the venue does not run — and
    // nothing is built for it; an account with no pair is the ordinary, silent paper state.
    DeribitVenueAdapter::say_what_the_live_tier_is(&req.inputs);
    let Resolution::Armed { .. } = DeribitVenueAdapter::resolution(&req.inputs) else {
        return AdapterOutcome::paper();
    };
    let Some(c) = DeribitVenueAdapter::credentials(&req.inputs) else {
        return AdapterOutcome::paper();
    };
    tracing::warn!(
        venue = VENUE,
        account = %req.inputs.account,
        tier = AccountTier::Demo.as_str(),
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
    // reconciliation is off — the factory is not called, so such a start does no authenticated
    // network work here.
    let recon = recon_if_enabled(req.recon_enabled, || connect_recon(&c, req.symbol));
    AdapterOutcome {
        exec: ExecOutcome::Live(LiveExec {
            client: spawn(c, req.symbol.to_string(), req.events.clone(), req.properties_rec),
            bound_tier: AccountTier::Demo,
            grid,
            contract_size,
            margin_mode: None,
            leg_grids: Vec::new(),
        }),
        recon,
        identity: None,
    }
}

impl VenueAdapter for DeribitVenueAdapter {
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

    fn resolve(&self, inputs: &AdapterInputs<'_>) -> Resolution {
        Self::resolution(inputs)
    }

    fn start(&self, req: AdapterRequest<'_>) -> AdapterOutcome {
        // Read BEFORE `req` moves into `start_with`; the spawn closure hands it to the client.
        let halt_path = req.inputs.process.halt_path.clone();
        start_with(
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

    /// A labelled account is started only when it armed, so a `live` one holding its pair never
    /// reaches `start`; `vike-tradehub-core` asks for what `start` would have said (see the trait method).
    fn report_not_running_account(&self, inputs: &AdapterInputs<'_>) {
        Self::say_what_the_live_tier_is(inputs);
    }

    /// `/api/v2/public/get_time` on the TESTNET host — unconditionally, because every exec spawn
    /// site binds `TESTNET_REST` and deribit has no mainnet switch. A check pointed at
    /// `www.deribit.com` would measure a host this start never talks to.
    fn server_time_ms(
        &self,
        _inputs: &AdapterInputs<'_>,
        timeout: Duration,
    ) -> Result<i64, String> {
        parse_server_time(&bounded_public_get(
            VENUE,
            crate::transport::TESTNET_REST,
            crate::transport::PATH_TIME,
            timeout,
        )?)
    }
}

#[path = "adapter_tests.rs"]
#[cfg(test)]
mod adapter_tests;
