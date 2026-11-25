//! deribit's VENUE ADAPTER — the `vike_bridge_core::venue_adapter::VenueAdapter` contract over the
//! testnet exec client and its dedicated authed order-WS reconcile client
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md).
//!
//! Moved, behaviour unchanged, from `vike-tradehub-core`: the `("deribit", Some(c))` arm, its row of the
//! legacy arming probe, its clock row WITH the read and parse (`deribit_time`,
//! `parse_deribit_time`), its book-identity row and its grid-source row. Its fallback grid is gone (owner,
//! 2026-11-13): an instrument whose row has not loaded is refused in words.
//!
//! Credentials are the generic pair — the started ACCOUNT's `API_KEY` + `API_SECRET` fields
//! (`AdapterInputs::credentials`, decision 0156 item 6) — and this is a DEMO-ONLY arm: deribit has no
//! `{VENUE}_MAINNET`-shaped switch, its network is not the account tier's (decision 0095 hands that
//! to binance/bybit/okx and hyperliquid alone), and every exec spawn site binds the testnet's
//! `venue_url` rows (`demo` `trade`, [`DeribitUrls`]) whatever the tier. So a `demo` account holding its
//! pair arms the testnet, held below live as `ArmingHeldBelowLive::DemoOnlyArm`; a row that is missing, or a
//! demo row naming the live host, leaves it NOT RUNNING with the row's words.
//!
//! ⚠ **A LIVE ACCOUNT'S KEY SET IS NAMED, NOT IGNORED.** An account whose tier is `live` and which
//! holds a complete pair would otherwise read like an empty store. It answers
//! `ArmingNotRunningCause::LiveTierNotWired` and `start` says so ONCE at `error!`
//! (`vike_bridge_core::venue_adapter::report_live_tier_not_wired`), then does not run: no venue starts
//! live that did not before. The arm is the shared `vike_bridge_core::venue_adapter::ArmingDemoOnly`
//! (`VenueAdapterDeribit::arm`): its `report_live_tier` is what the start says, and it speaks for a
//! labelled account that is never started too ([`VenueAdapter::report_not_running_account`]), so the
//! two cannot word one fact two ways. An account with no complete pair is `NoCredentials`, silently, at
//! either tier.

use std::sync::Arc;
use std::time::Duration;

use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::venue_adapter::{
    AdapterInputs, AdapterOutcome, AdapterRequest, ArmingDemoOnly, ArmingResolution,
    BookIdentityRule, ClockAuthentication, ClockDeclaration, VenueAdapter, VenueAdapterDeclaration,
    VenueAdapterExecutionOutcome, VenueAdapterExecutionRunning, bounded_public_get,
    missing_time_field, reconcile_if_enabled,
};
use vike_execution::recon::ReconcileClient;
use vike_execution::{EventSender, ExecutionClient};
use vike_model::{AccountTier, ClockRisk, GridSource, VenueRead, VenueReadKind};
use vike_secrets::credentials::{Credentials, api_credentials};
use vike_utility::errors::CarriedRefusal;
use vike_venues::venue_urls::UrlRefusal;

use crate::execution::DeribitProperties;
use crate::history::VENUE;
use crate::refusals::{DeribitClockRefusal, DeribitVenueReadRefusal};
use crate::transport::client::DeribitUrls;

/// deribit's JSON-RPC envelope. ⚠ The stamp is a BARE top-level `result` i64 in epoch MS,
/// sitting beside `usIn`/`usOut`/`usDiff` in MICROSECONDS — the nearest wrong field is a
/// thousand-fold out. The `"testnet"` boolean is a free correctness assert no other venue offers:
/// it proves the host that ANSWERED is the tier we bound.
fn parse_server_time(body: &serde_json::Value) -> Result<i64, DeribitClockRefusal> {
    if body.get("testnet").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err(DeribitClockRefusal::WrongHost);
    }
    body.get("result")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| DeribitClockRefusal::NoStamp { why: missing_time_field("result") })
}

/// deribit's start. `vike_tradehub::registry::REGISTRY` holds `&VenueAdapterDeribit`.
pub struct VenueAdapterDeribit;

impl VenueAdapterDeribit {
    /// **The demo-only arm** (`vike_bridge_core::venue_adapter::ArmingDemoOnly`), the one answer
    /// `resolve`, `start`, `report_not_running_account` and `venue_read` act on, over THIS account's key
    /// pair (its `API_KEY` and `API_SECRET` fields, `vike_secrets::credentials::api_credentials`) and the
    /// testnet's rows ([`DeribitUrls`]): a complete pair on a `demo` account (or `paper`, read as demo) arms
    /// the testnet, held below live as `ArmingHeldBelowLive::DemoOnlyArm` (a refused row is that arm's NOT
    /// RUNNING reason); a complete pair on a `live` account is `ArmingNotRunningCause::LiveTierNotWired`, since
    /// this arm has no live network to bind; no complete pair is `ArmingNotRunningCause::NoCredentials` at either
    /// tier. The arm names no tier itself (`crates/vike-ops/tests/venues/live_mount_line_gate.rs` reads
    /// a named live tier as an arm that can bind it).
    fn arm(
        inputs: &AdapterInputs<'_>,
    ) -> ArmingDemoOnly<Result<(Credentials, DeribitUrls), UrlRefusal>> {
        let pair = api_credentials(&inputs.credentials, VENUE);
        ArmingDemoOnly::of(
            inputs.tier,
            pair.map(|c| DeribitUrls::of(inputs.settings.urls()).map(|urls| (c, urls))),
        )
    }
}

/// The kind of a start read that failed, under deribit's own table (`crate::transport::error_codes::TAXONOMY`):
/// its `too_many_requests` (`crate::history::RATE_LIMIT_CODE`) a rate limit, asked again (owner 2026-11-16: the old
/// restart loop healed it), and a code the table does not map (`ErrorKind::UnmappedCode`) asked again too.
fn start_failure_kind(e: &VenueApiError) -> vike_bridge_core::transport::ErrorKind {
    vike_bridge_core::error_kind::classify_venue(Some(&crate::transport::error_codes::TAXONOMY), e)
}

/// [`VenueAdapterDeribit::start`]'s body, with its three NETWORK steps taken as parameters:
/// `fetch_grid` is the blocking keyless `public/get_instrument` pre-fetch, `connect_reconcile` the
/// reconcile client's authed order-WS handshake and `spawn` the exec client, each on the testnet
/// addresses ([`DeribitUrls`]) this body looks up in the account's rows. `start` passes the real three;
/// a test passes offline doubles, which is what lets `adapter_tests.rs` run the branch that arms a
/// demo account (every other branch of this body is already offline) — the only other way to reach
/// it is to dial the testnet host.
fn start_with(
    req: AdapterRequest<'_>,
    fetch_grid: impl FnOnce(&DeribitUrls, &str) -> Result<Option<DeribitProperties>, VenueApiError>,
    connect_reconcile: impl FnOnce(&DeribitUrls, &Credentials, &str) -> Option<Box<dyn ReconcileClient>>,
    spawn: impl FnOnce(
        DeribitUrls,
        Credentials,
        String,
        EventSender,
        Option<Arc<vike_data::PropertiesRecorder>>,
    ) -> Box<dyn ExecutionClient + Send>,
) -> AdapterOutcome {
    // A LIVE account holding its pair is said out loud — it is why the venue does not run — and
    // nothing is built for it; an account with no pair is the ordinary, silent paper state. The
    // testnet's rows (`demo` `trade`) were looked up BEFORE anything is dialled: a missing row, or a
    // demo row that names the live host, is the account's NOT RUNNING reason, said once, and nothing
    // is built.
    let Some((c, urls)) =
        VenueAdapterDeribit::arm(&req.inputs).config_for_start(VENUE, req.inputs.account)
    else {
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
    // startup-only), which also says what a unit of the quantity is WORTH (decision 0170): the same
    // row decides the grid and the contract value, so they cannot disagree about one instrument.
    //
    // ⚠ NO ROW, NO VALUE, NO MOUNT. Deribit lists coin-margined contracts whose `amount` is dollars
    // (`BTC-PERPETUAL`) beside linear ones whose `amount` is the base coin (`BTC_USDC-PERPETUAL`), and
    // the two are valued differently; a fetch that failed, a future that states no margining and a
    // quanto row all leave the kind UNKNOWN, and an unknown kind is never defaulted to `Linear` for
    // a venue that could be inverse. The account does not run, with the reason: a read a later try can
    // pass (a testnet blip: a network failure, a 5xx, a rate limit) is asked again by itself
    // (`AdapterOutcome::failed`), and a row the venue refuses or words differently waits for the operator.
    //
    // The mounted symbol is a USER name (decision 0290: `BTCUSD.P`); the venue is asked for its KEPT row's name
    // (`BTC-PERPETUAL`). No list or no row, no mount: the account does NOT RUN, with the reason (a deribit list is
    // kept by `vike-cli data catalog refresh --venue deribit`). The list is read as it is NOW (a cold start), so
    // an operator's refresh asks the parked account again and that start reads it.
    let _ = vike_catalog::kept_list_now(VENUE);
    let venue_name = match crate::names::venue_name_of(req.symbol) {
        Ok(venue_name) => venue_name,
        Err(why) => {
            tracing::error!(
                venue = VENUE,
                account = %req.inputs.account,
                symbol = req.symbol,
                "deribit: REFUSED — {why}; the account is NOT RUNNING until the kept list holds it"
            );
            return AdapterOutcome::paper();
        }
    };
    let facts = match fetch_grid(&urls, &venue_name) {
        Ok(Some(facts)) => facts,
        unread => {
            let failed = unread.err();
            tracing::error!(
                venue = VENUE,
                account = %req.inputs.account,
                symbol = req.symbol,
                error = ?failed,
                "deribit: REFUSED — the instrument row could not be fetched, so whether {} counts its \
                 amount in dollars or in coin cannot be known and a guess would size every order at \
                 the wrong notional; the account is NOT RUNNING until the row can be read",
                req.symbol
            );
            return failed.map_or_else(AdapterOutcome::paper, |e| {
                AdapterOutcome::failed(start_failure_kind(&e))
            });
        }
    };
    let value = match facts.value {
        Ok(value) => value,
        Err(why) => {
            tracing::error!(
                venue = VENUE,
                account = %req.inputs.account,
                symbol = req.symbol,
                "deribit: REFUSED — {why}; the account is NOT RUNNING"
            );
            return AdapterOutcome::paper();
        }
    };
    let grid = Some(facts.grid);
    // `contract_size` stays on the grid as the LOT and is no money (decision 0170, item 4).
    let contract_value = Some(value);
    // Reconcile handle (audit A1 item 4): `crate::reconcile`'s `recon_client` opens its OWN
    // dedicated authed order-WS (a blocking JSON-RPC auth), so reconcile report fetches never
    // contend the exec side's order-transport `Mutex` — an order submit is never delayed behind
    // a reconcile round-trip. Built from `&c` before `c` moves into the spawn. `None`
    // (auth/connect fail) stays reconcile-inert, exec unaffected.
    //
    // LAZY (`req.recon_enabled`): that blocking authed handshake is skipped entirely when
    // reconciliation is off — the factory is not called, so such a start does no authenticated
    // network work here.
    let recon =
        reconcile_if_enabled(req.reconcile_enabled, || connect_reconcile(&urls, &c, &venue_name));
    AdapterOutcome {
        exec: VenueAdapterExecutionOutcome::Running(VenueAdapterExecutionRunning {
            client: spawn(urls, c, req.symbol.to_string(), req.events.clone(), req.properties_rec),
            bound_tier: AccountTier::Demo,
            grid,
            contract_value,
            margin_mode: None,
            leg_grids: Vec::new(),
        }),
        reconcile: recon,
        identity: None,
    }
}

impl VenueAdapter for VenueAdapterDeribit {
    fn venue(&self) -> &'static str {
        VENUE
    }

    fn declaration(&self) -> VenueAdapterDeclaration {
        VenueAdapterDeclaration {
            addresses_accounts: true,
            process_exclusive: None,
            // `crate::execution`'s `fetch_deribit_instrument`: a symbol-scoped blocking pre-fetch.
            grid_source: GridSource::PerSymbolFetch,
            book_identity: BookIdentityRule::Undeterminable {
                why: "the store holds a client id/secret pair; deribit's subaccount is selected \
                      by the credential and named nowhere in the store",
            },
            // Deribit's own auth carries no timestamp — see `crate::transport::client`'s `PATH_TIME`.
            // Wired anyway: it is keyless, it answers in ~50 ms from the CI box, and it is the only
            // venue that tells us WHICH HOST answered.
            clock: ClockDeclaration::Wired {
                endpoint: "GET /api/v2/public/get_time (public, testnet host)",
                auth: ClockAuthentication::Public,
                risk: ClockRisk::NoTimestamp,
            },
        }
    }

    fn resolve(&self, inputs: &AdapterInputs<'_>) -> ArmingResolution {
        Self::arm(inputs).resolution()
    }

    fn start(&self, req: AdapterRequest<'_>) -> AdapterOutcome {
        // Read BEFORE `req` moves into `start_with`; the spawn closure hands it to the client.
        let halt_path = req.inputs.process.halt_path.clone();
        start_with(
            req,
            |urls, symbol| crate::execution::fetch_deribit_instrument(&urls.rest, symbol),
            crate::reconcile::recon_client,
            move |urls, creds, symbol, events, properties_rec| {
                Box::new(
                    crate::execution::spawn_deribit_execution_client(
                        urls,
                        creds,
                        symbol,
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
        Self::arm(inputs).report_live_tier(VENUE, inputs.account);
    }

    /// `/api/v2/public/get_time` on the TESTNET host — unconditionally, because every exec spawn
    /// site binds the testnet REST row (`crate::transport::client`'s `testnet_rest`) and deribit has no mainnet
    /// switch. A check pointed at `www.deribit.com` would measure a host this start never talks to.
    fn server_time_ms(&self, inputs: &AdapterInputs<'_>, timeout: Duration) -> Result<i64, String> {
        let rest = crate::transport::client::testnet_rest(inputs.settings.urls())
            .map_err(|e| e.to_string())?;
        parse_server_time(&bounded_public_get(
            VENUE,
            rest,
            crate::transport::client::PATH_TIME,
            timeout,
        )?)
        .map_err(|why| why.to_string())
    }

    /// An operator's READ of the account ([`crate::reconcile::venue_read`]), for the account this arm would start:
    /// every authed socket here is the testnet's, so a `live` account's pair has no host to ask, and it
    /// is told so with nothing sent.
    fn venue_read(
        &self,
        inputs: &AdapterInputs<'_>,
        kind: VenueReadKind,
    ) -> Result<VenueRead, CarriedRefusal> {
        match Self::arm(inputs) {
            ArmingDemoOnly::Demo(Ok((creds, urls))) => {
                crate::reconcile::venue_read::venue_read(&urls.ws, &creds, kind)
                    .map_err(|why| why.carried())
            }
            ArmingDemoOnly::Demo(Err(why)) => Err(why.to_string().into()),
            ArmingDemoOnly::LiveTierNotWired => {
                Err(DeribitVenueReadRefusal::LiveTierNotWired.carried())
            }
            ArmingDemoOnly::Unconfigured => Err(DeribitVenueReadRefusal::Unconfigured.carried()),
        }
    }
}

#[path = "adapter_tests.rs"]
#[cfg(test)]
mod adapter_tests;
