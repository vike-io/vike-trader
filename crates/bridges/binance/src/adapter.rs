//! binance's VENUE ADAPTER — the `vike_bridge_core::venue_adapter::VenueAdapter` contract over the spot
//! and USDⓈ-M perp exec client, its reconcile client and the withdraw-permission gate that guards
//! them (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md).
//!
//! Moved, behaviour unchanged, from `vike-tradehub-core`: the `("binance", Some(c))` arm and the pre-match
//! withdraw gate that guarded it (now `start_with`, which [`VenueAdapterBinance::start`] calls, and
//! `vike_secrets::key_permissions::withdraw_gate_before_start`), its row of the legacy arming probe ([`VenueAdapterBinance::resolve`]),
//! its clock, book-identity and grid-source rows ([`VenueAdapterBinance::declaration`]), its clock
//! read and parse ([`VenueAdapterBinance::server_time_ms`]), its `AUTHED_READ_MARKETS` startup probe
//! ([`VenueAdapterBinance::credential_probe`]) and its arm of the `build_recon_client` dispatch. Its
//! fallback grid is gone (owner, 2026-11-13): a symbol whose grid has not loaded is refused in words.
//!
//! # The account's tier decides the network (D1)
//!
//! `live` means mainnet (docs/decisions/0095-venues-read-no-environment-and-live-means-mainnet.md):
//! a `live` account binds the mainnet hosts, every lower tier binds the demo hosts, and there is no
//! switch beside the tier. The key pair is the started ACCOUNT's (`AdapterInputs::credentials`, its
//! `API_KEY` + `API_SECRET` fields — decision 0156 item 6): an account has ONE tier, so its keys
//! are that tier's and no name is composed to find them. So [`AdapterInputs::tier`] (through
//! `vike_model::VenueNetwork::for_tier`) is the whole tier answer — for the withdraw gate, the grid
//! pre-fetch, the exec hosts, the reconcile client, the clock read and the tier handed to the identity record (inert
//! for binance today — see `credential_probe`) alike — and a `live` account with no key pair does NOT RUN:
//! there is no other account's pair to fall back to.
//!
//! # `.P` routing
//!
//! One venue id, two exec lanes: a trailing `.P` on the engine symbol selects the USDⓈ-M perp,
//! anything else spot; a `.P` COIN-M name (`BTCUSD_PERP.P`) is the perp lane on the `dapi` host, and its start
//! reads its money from its own row (`crate::execution::rest::coin_m`'s `fetch_mount`). Nothing else here routes. `crate::execution`'s `fetch_binance_properties` and
//! `spawn_with_recorder` and `crate::reconcile`'s `recon_client` each split the suffix
//! themselves, the same way, so one start's grid, exec client and reconcile client cannot land on
//! different lanes.
//!
//! # The withdraw gate
//!
//! The withdraw gate (`vike_secrets::key_permissions::withdraw_gate_before_start`, the one gate every venue
//! whose API reports key permissions runs) is this start's one decline after `resolve` armed: it needs a signed
//! network read, so it cannot live in the pure probe. It runs before anything is built — the grid
//! pre-fetch, the reconcile client and the exec spawn all come after it — which is where the legacy
//! arm's guard put it, and `vike-tradehub-core`'s pre-connect budget refusal has already run by the time
//! `start` is called, as it had before that guard. The override is `flags.allow_withdraw_keys`, handed to
//! the start as a bool (`AdapterInputs::flags`, filled by `crates/vike-gatewayhub/src/trade/adapters/contract.rs`'s
//! `adapter_flags`): no credential-store row and no process environment can arm it.
//!
//! # The network steps are parameters
//!
//! `start_with` is the start's body with its three NETWORK steps — the withdraw gate's signed
//! permission read, the grid pre-fetch and the exec spawn whose thread dials the venue — taken as
//! parameters. [`VenueAdapterBinance::start`] passes the real three; this module's tests pass
//! offline doubles, so they read what the body binds, spawns and declines rather than what a helper
//! beside it answers. Nothing else in the body reaches a socket: `crate::reconcile`'s
//! `recon_client` only constructs.

use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::Duration;

use vike_bridge_core::leverage_read::LeverageWriteError;
use vike_bridge_core::reconcile_admitted::ReconcileAdmittedSymbols;
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::venue_adapter::{
    AdapterInputs, AdapterOutcome, AdapterRequest, ArmingResolution, BookIdentityRule,
    ClockAuthentication, ClockDeclaration, CredentialProbe, VenueAdapter, VenueAdapterDeclaration,
    VenueAdapterExecutionOutcome, VenueAdapterExecutionRunning, bound_tier, bounded_public_get,
    report_address_refused, report_live_credentials_absent, resolve_one_key_set,
};
use vike_execution::{EventSender, ExecutionClient};
use vike_model::{AccountTier, ClockRisk, GridSource, SymbolProperties, VenueNetwork};
use vike_secrets::credentials::{Credentials, api_credentials, attribution_code_from};
use vike_secrets::key_permissions::{
    CredentialWithdrawGate, KeyPermissionProbe, KeyPermissions, withdraw_gate_before_start,
};

use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlRefusal, UrlType};

use crate::VENUE;
use crate::execution::BinanceUrls;
use crate::family::execution::rest_reads::parse_server_time;
use crate::refusals::BinanceAccountRefusal;
use vike_bridge_core::signer::VenueClock;

/// **The account's six exec addresses** ([`BinanceUrls::resolve`] over its `venue_url` rows,
/// `AdapterInputs::settings`), at the tier its network binds (`bound_tier` of
/// `VenueNetwork::for_tier`: `Paper` reads as `Demo`) — the one lookup the start, the leverage write
/// and the mode heal share.
fn hosts_of(inputs: &AdapterInputs<'_>) -> Result<BinanceUrls, UrlRefusal> {
    BinanceUrls::resolve(inputs.settings.urls(), bound_tier(VenueNetwork::for_tier(inputs.tier)))
}

/// `crate::execution`'s `spawn_with_recorder` arguments, in its order, as
/// `start_with` assembles them — the one value its spawn step is handed.
struct ExecutionSpawn {
    hosts: BinanceUrls,
    /// The PUBLIC spot address the BNB fee rate is read from: the account's type at channel `data`, so a
    /// demo start reads the MAINNET ticker as it always did (a price, not an account). A spot pair's quote
    /// asset is read there too (its `exchangeInfo` row).
    ticker_rest: &'static str,
    /// The PUBLIC USDⓈ-M address a perpetual's quote asset is read from (its `exchangeInfo` row), at the same
    /// type and channel as `ticker_rest`.
    usdm_list_rest: &'static str,
    creds: Credentials,
    symbol: String,
    events: EventSender,
    properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
    on_reconcile: Option<Sender<()>>,
    link_id: Option<String>,
    leverage: f64,
    trade_lite_fill: bool,
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

/// [`VenueAdapterBinance::start`]'s body, with its NETWORK steps taken as parameters (module
/// doc): `fetch_permissions` is the withdraw gate's signed read (handed the live spot address),
/// `fetch_grid` the grid pre-fetch, `fetch_coin_m` a COIN-M mount's own `dapi` row
/// (`crate::execution::rest::coin_m::fetch_mount`, handed the account's `dapi` address, asked for a COIN-M name only) and
/// `spawn` the exec client whose thread dials the venue.
///
/// The account leverage it posts is `vike_bridge_core::leverage::leverage_for` over the operator's `[risk]` budget,
/// resolved ONCE here and threaded into the spawn, so the venue account and the RiskGate size on one number.
/// ⚠ **NOT clamped to a venue ceiling, unlike bybit/okx** — `vike_bridge_core::leverage::clamp_to_venue_cap` exists
/// and this arm deliberately does not call it, because binance publishes no readable cap on any response this adapter
/// already fetches. VERIFIED LIVE 2026-08-05: `GET /fapi/v1/exchangeInfo` (1.0 MB, every symbol) contains ZERO
/// case-insensitive `leverage` matches, and the real ceiling lives in the leverage BRACKETS at
/// `/fapi/v1/leverageBracket`, which answers `{"code":-2014,"msg":"API-key format invalid."}` unauthenticated — i.e. a
/// WHOLE NEW SIGNED request on the start path, not a field ride-along. Those brackets are also RISK-TIERED
/// (`initialLeverage` falls as position notional rises), so "the cap" here is a function of size, not the single flat
/// number bybit/okx publish; clamping to the tier-0 value would be a weaker and different claim. Adding that fetch is
/// a deliberate follow-up with its own cost discussion, not a doc-closing change — so a too-high request is still
/// rejected by the VENUE, which `run_perp`'s `set_leverage` warn already surfaces.
fn start_with(
    req: AdapterRequest<'_>,
    fetch_permissions: impl FnOnce(&'static str, &Credentials) -> Result<KeyPermissions, String>,
    fetch_grid: impl FnOnce(BinanceUrls, &Credentials, &str) -> Option<SymbolProperties>,
    fetch_coin_m: impl FnOnce(
        &'static str,
        &str,
    ) -> Result<crate::execution::rest::coin_m::CoinMMount, VenueApiError>,
    spawn: impl FnOnce(ExecutionSpawn) -> Box<dyn ExecutionClient + Send>,
) -> AdapterOutcome {
    let Some(c) = VenueAdapterBinance::credentials(&req.inputs) else {
        report_live_credentials_absent(VENUE, req.inputs.account, req.inputs.tier);
        return AdapterOutcome::paper();
    };
    // Decision 0095: for this venue the tier IS the network. `bound == AccountTier::Live` is the
    // real-money arm, spelled here because the announcement's gate (below) reads it.
    let bound = bound_tier(VenueNetwork::for_tier(req.inputs.tier));
    let mainnet = bound == AccountTier::Live;
    // The addresses are the account's `venue_url` rows at its tier ([`hosts_of`]), looked up ONCE here,
    // before anything dials: a missing row (or a demo row that is the live address) means the account
    // does NOT RUN, with the row named.
    let resolved = hosts_of(&req.inputs).and_then(|hosts| {
        let urls = req.inputs.settings.urls();
        let data = |product| {
            urls.url(hosts.ty, UrlChannel::Data, UrlProtocol::Rest, product)
                .map(crate::family::static_url)
        };
        Ok((hosts, data("spot")?, data("usdm")?))
    });
    let (hosts, ticker_rest, usdm_list_rest) = match resolved {
        Ok(found) => found,
        Err(refusal) => {
            report_address_refused(VENUE, req.inputs.account, refusal);
            return AdapterOutcome::paper();
        }
    };
    // THE WITHDRAW GATE, before anything is built: one signed read on mainnet, none on demo (`sapi` is
    // mainnet-only, so a demo read could only fail). A `Refuse` builds nothing — no grid fetched, no
    // reconcile client, nothing spawned — and the account does NOT RUN: no paper client takes its place
    // (decision 0156).
    let gate = withdraw_gate_before_start(
        VENUE,
        &req.inputs.account.to_string(),
        mainnet,
        req.inputs.flags.allow_withdraw_keys,
        "this API key",
        "Re-issue a trade-only key",
        || fetch_permissions(hosts.spot_rest, &c),
    );
    if gate == CredentialWithdrawGate::Refuse {
        return AdapterOutcome::paper();
    }
    // The announcement convention — `venue`, `account`, `tier`, and `⚠ REAL-MONEY: ` on a live tier
    // only — is held for every bridge by `crates/vike-ops/tests/venues/live_mount_line_gate.rs`.
    if mainnet {
        tracing::warn!(
            venue = VENUE,
            account = %req.inputs.account,
            tier = AccountTier::Live.as_str(),
            "⚠ REAL-MONEY: binance mounting on MAINNET with LIVE credentials (real funds)"
        );
    } else {
        tracing::warn!(
            venue = VENUE,
            account = %req.inputs.account,
            tier = AccountTier::Demo.as_str(),
            "binance: DEMO credentials present → LIVE exec client (real demo orders)"
        );
    }
    // Live RiskGate from the venue's REAL grid (PR-2b): one blocking pre-fetch (spot /api/v3 or
    // fapi /fapi/v1 exchangeInfo, routed inside by the `.P` suffix — a duplicate of the exec
    // thread's own, acceptable startup-only). On failure the permissive default stands.
    //
    // A COIN-M mount (decisions 0170, 0258) reads its own `dapi` row instead, which also says what a unit of
    // the quantity is WORTH: `Inverse(contractSize)`, from which the engine keeps its books in the row's settle
    // coin. ⚠ NO ROW, NO MOUNT: a COIN-M name's kind and face are never defaulted (a linear 1.0, the common
    // face of 100), so the account does NOT RUN, with the reason, as bybit's and deribit's starts do: a read a
    // later try can pass (a network failure, a 5xx, a rate limit) is asked again by itself, a row the list
    // refuses waits for the operator (`AdapterOutcome::failed`). A USDⓈ-M or spot mount reports no contract
    // value, as before.
    //
    // The mounted symbol is a USER name (decision 0290: `BTCUSD.P`, `BTCUSDT_261225`); the bridge speaks its KEPT
    // row's lane name (`BTCUSD_PERP.P`, `BTCUSDT_261225.P`), whose `.P` picks the futures book. No list or no row, no
    // mount: the account does NOT RUN, with the reason. The list is read as it is NOW (a cold start), so an
    // operator's refresh asks the parked account again and that start reads it.
    let _ = vike_catalog::kept_list_now(VENUE);
    let lane = match crate::instrument::names::lane_name_of(req.symbol) {
        Ok(lane) => lane,
        Err(why) => {
            tracing::error!(
                venue = VENUE,
                account = %req.inputs.account,
                symbol = req.symbol,
                "binance: REFUSED — {why}; the account is NOT RUNNING until the kept list holds it"
            );
            return AdapterOutcome::paper();
        }
    };
    let (grid, contract_value) = if crate::execution::rest::coin_m::names_coin_m(&lane) {
        match fetch_coin_m(hosts.dapi_rest, &lane) {
            Ok(mount) => (Some(mount.properties), Some(mount.value)),
            Err(why) => {
                let kind = vike_bridge_core::error_kind::classify_venue(
                    Some(&crate::transport::error_codes::TAXONOMY),
                    &why,
                );
                tracing::error!(
                    venue = VENUE,
                    account = %req.inputs.account,
                    symbol = req.symbol,
                    kind = %kind,
                    "binance: REFUSED — the COIN-M contract cannot be mounted: {}; the account is NOT \
                     RUNNING until its row can be read",
                    why.msg
                );
                return AdapterOutcome::failed(kind);
            }
        }
    } else {
        (fetch_grid(hosts, &c, &lane), None)
    };
    // Reconcile handle: a FRESH HMAC REST client dedicated to reconcile reads, built from `&c`
    // before `c` moves into the spawn, on the SAME `.P` lane and the SAME resolved `mainnet` the
    // exec client binds, so it reconciles the account these credentials authenticate. Pure to
    // construct, so NOT gated on `recon_enabled` — exactly as the legacy arm built it.
    // Fee-attribution Broker/Link id (`BINANCE_BROKER_CODE`/`BINANCE_BUILDER_CODE`), validated
    // against binance's `AttributionMechanic`. Absent/invalid degrades to `None` — the wire body's
    // `newClientOrderId`/`origClientOrderId` then carry the bare local order client id. The reconcile client
    // takes it too: it names a triggered stop's fill only when the stop's id is one of OURS.
    let link_id = attribution_code_from(req.inputs.secrets, VENUE);
    // The admitted-symbol list is ONE `Arc`: the reconcile client is told each admission
    // (`ReconcileClient::admit_symbol`), and the exec thread's reconnect-resync and gap-sentinel read the same
    // list, so a symbol the engine admits is also one whose missed fills are replayed (decision 0174's
    // reopener).
    let admitted = Arc::<ReconcileAdmittedSymbols>::default();
    // One clock offset too: the exec thread reads it at its start, and this client signs on it.
    let clock = VenueClock::default();
    let recon = crate::reconcile::reconcile_client_sharing(
        &c,
        &lane,
        hosts,
        link_id.clone(),
        Arc::clone(&admitted),
        &clock,
    );
    // ACCOUNT LEVERAGE from the operator's `[risk]` budget; unset ⇒ the historical 2.0. Inert on a
    // spot symbol. UNCLAMPED, unlike bybit/okx — this function's doc carries why.
    let leverage = vike_bridge_core::leverage::leverage_for(req.risk_profile);
    // The TRADE_LITE early-fill hint — `venue.binance.trade_lite_fill` (decision 0095), read once
    // from the venue's settings.
    let trade_lite_fill = crate::execution::user_data::perp::trade_lite_fill(req.inputs.settings);
    AdapterOutcome {
        exec: VenueAdapterExecutionOutcome::Running(VenueAdapterExecutionRunning {
            client: spawn(ExecutionSpawn {
                hosts,
                ticker_rest,
                usdm_list_rest,
                creds: c,
                symbol: req.symbol.to_string(),
                events: req.events.clone(),
                properties_rec: req.properties_rec,
                on_reconcile: req.reconcile_trigger,
                link_id,
                leverage,
                trade_lite_fill,
                admitted,
                clock,
                halt_path: req.inputs.process.halt_path.clone(),
            }),
            bound_tier: bound,
            grid,
            contract_value,
            margin_mode: None,
            leg_grids: Vec::new(),
        }),
        reconcile: recon,
        identity: None,
    }
}

/// binance's start. `vike_tradehub::registry::REGISTRY` holds `&VenueAdapterBinance`.
pub struct VenueAdapterBinance;

impl VenueAdapterBinance {
    /// The one gate `resolve`, `start` and the startup probe share: THIS account's key pair — its
    /// `API_KEY` and `API_SECRET` fields (`vike_secrets::credentials::api_credentials`), which are
    /// its tier's because the account has one tier.
    fn credentials(inputs: &AdapterInputs<'_>) -> Option<Credentials> {
        api_credentials(&inputs.credentials, VENUE)
    }
}

impl VenueAdapter for VenueAdapterBinance {
    fn venue(&self) -> &'static str {
        VENUE
    }

    fn declaration(&self) -> VenueAdapterDeclaration {
        VenueAdapterDeclaration {
            addresses_accounts: true,
            process_exclusive: None,
            // The resync supervisor (spot or perp) fires the recon trigger after every
            // reconnect's event replay settles.
            // `crate::execution`'s `fetch_binance_properties`: one symbol-scoped blocking
            // `exchangeInfo` pre-fetch (`?symbol=`), spot or fapi by the `.P` suffix.
            grid_source: GridSource::PerSymbolFetch,
            book_identity: BookIdentityRule::Undeterminable {
                why: "the store holds an HMAC api key/secret pair and no account identifier; which \
                      account a key belongs to is answerable only by an authenticated call",
            },
            clock: ClockDeclaration::Wired {
                endpoint: "GET /api/v3/time (public)",
                auth: ClockAuthentication::Public,
                risk: ClockRisk::SignedTimestamp,
            },
        }
    }

    /// The account's key pair arms the account's tier; `held_below_live` is always `None`, because
    /// DEMO is reached only under a tier below `live`, where nothing is held below it. With no
    /// complete pair the account does NOT RUN — and under a `live` tier the cause is the missing
    /// LIVE key set (D1): the account has one set, so there is nothing to fall back to.
    fn resolve(&self, inputs: &AdapterInputs<'_>) -> ArmingResolution {
        resolve_one_key_set(
            Self::credentials(inputs).is_some(),
            VenueNetwork::for_tier(inputs.tier),
        )
    }

    fn start(&self, req: AdapterRequest<'_>) -> AdapterOutcome {
        start_with(
            req,
            |spot_rest, c| {
                crate::account::identity::key_permission_probe(c, spot_rest).fetch_key_permissions()
            },
            |hosts, c, symbol| {
                crate::execution::fetch_binance_properties(hosts, c, symbol)
                    .ok()
                    .map(|(f, _base)| f)
            },
            crate::execution::rest::coin_m::fetch_mount,
            |s| {
                // The spot fee rule prices a BNB-discounted fee through the PUBLIC mainnet ticker (a
                // price, not an account: demo and live accounts value the same BNB; `ExecutionSpawn::ticker_rest`)
                // in the quote the fill's own instrument row names (`ExecutionSpawn::usdm_list_rest` for a
                // perpetual), registered once per process and only by a start that really spawns, so no
                // mapper test opens a socket.
                crate::family::execution::spot_fee::register_public_ticker_rates(
                    VENUE,
                    s.ticker_rest,
                    s.usdm_list_rest,
                );
                Box::new(
                    crate::execution::spawn_with_recorder(
                        s.hosts,
                        s.creds,
                        s.symbol,
                        s.events,
                        s.properties_rec,
                        s.on_reconcile,
                        s.link_id,
                        s.leverage,
                        s.trade_lite_fill,
                        s.admitted,
                        s.clock,
                    )
                    .with_halt_path(s.halt_path),
                )
            },
        )
    }

    /// `/api/v3/time` on the SAME demo/mainnet host the start binds — the account's tier (D1), so
    /// the reading is against the clock that judges this start's signed requests.
    fn server_time_ms(&self, inputs: &AdapterInputs<'_>, timeout: Duration) -> Result<i64, String> {
        let base = inputs
            .settings
            .urls()
            .url(UrlType::of_tier(inputs.tier), UrlChannel::Trade, UrlProtocol::Rest, "spot")
            .map_err(|refusal| refusal.to_string())?;
        parse_server_time(&bounded_public_get(
            VENUE,
            base,
            crate::execution::rest::spot::PATH_TIME,
            timeout,
        )?)
    }

    /// Was `vike-tradehub-core`'s legacy `authed_read_probes` over its `AUTHED_READ_MARKETS` row for this
    /// venue: the probed account's pair (`vike-tradehub-core` asks only for a venue this box would arm,
    /// and only on the default account's behalf) and a reconcile client built PURELY — a signer
    /// plus a transport. `vike-tradehub-core` reads the balance through it, then hands it to the identity
    /// record at `bound_tier`, the tier the credentials authenticate. That record is inert for binance
    /// today: `crate::reconcile`'s `ReconcileClientBinance` does not forward `fetch_account_identity`,
    /// so the trait's default answers `Ok(None)` and nothing is recorded. No address row for the tier: the
    /// error is logged and the probe FAILS in the row's words (`CredentialProbe::failing`, owner ruling B-19),
    /// so the credential leg refuses the account loudly instead of saying nothing (the start refuses it with
    /// the same words).
    fn credential_probe(&self, inputs: &AdapterInputs<'_>) -> Option<CredentialProbe> {
        let creds = Self::credentials(inputs)?;
        let hosts = match hosts_of(inputs) {
            Ok(hosts) => hosts,
            Err(refusal) => {
                tracing::error!(venue = VENUE, "binance: no credential probe: {refusal}");
                return Some(CredentialProbe::failing(refusal.to_string()));
            }
        };
        // No symbol (owner, 2026-11-13: none is compiled in, and the venue needs none here): the probe reads the
        // account-wide balance alone, which no symbol scopes, on the spot lane a name without `.P` selects.
        let client = crate::reconcile::recon_client(&creds, "", hosts)?;
        Some(CredentialProbe::RecordsIdentity {
            client,
            bound_tier: bound_tier(VenueNetwork::for_tier(inputs.tier)),
        })
    }

    /// The account's leverage for a perpetual the engine was not mounted on: the exec half's own
    /// set-leverage ([`crate::execution::write_symbol_leverage`]) with THIS account's key pair, on the fapi
    /// host its tier selects and with the broker link a start resolves. A SPOT symbol and an account
    /// with no key pair are refused without a call.
    fn set_symbol_leverage(
        &self,
        inputs: &AdapterInputs<'_>,
        symbol: &str,
        leverage: f64,
    ) -> Result<(), LeverageWriteError> {
        let creds = Self::credentials(inputs).ok_or_else(|| {
            LeverageWriteError::Refused(BinanceAccountRefusal::NoKeyPair.to_string())
        })?;
        let hosts =
            hosts_of(inputs).map_err(|refusal| LeverageWriteError::Refused(refusal.to_string()))?;
        crate::execution::write_symbol_leverage(
            &creds,
            hosts,
            symbol,
            leverage,
            attribution_code_from(inputs.secrets, VENUE),
        )
    }

    /// The account's position mode and the symbol's margin type put right BEFORE the gatewayhub's probe
    /// reads them: the start's own switch ([`crate::execution::heal_symbol_modes`]) with THIS account's key
    /// pair, on the fapi host its tier selects. No key pair, no call.
    fn heal_modes(&self, inputs: &AdapterInputs<'_>, symbol: &str) -> Result<(), String> {
        let creds = Self::credentials(inputs)
            .ok_or_else(|| BinanceAccountRefusal::NoKeyPair.to_string())?;
        crate::execution::heal_symbol_modes(
            &creds,
            hosts_of(inputs).map_err(|refusal| refusal.to_string())?,
            symbol,
            attribution_code_from(inputs.secrets, VENUE),
        )
    }

    /// An operator's READ of the account (`crate::reconcile::venue_read`): every open order of its spot, USDⓈ-M and COIN-M
    /// books, with THIS account's key pair on the hosts its tier selects ([`hosts_of`]). It writes nothing. No key
    /// pair, nothing asked; the balance and the positions are not offered.
    fn venue_read(
        &self,
        inputs: &AdapterInputs<'_>,
        kind: vike_model::VenueReadKind,
    ) -> Result<vike_model::VenueRead, vike_utility::errors::CarriedRefusal> {
        let creds = Self::credentials(inputs)
            .ok_or_else(|| BinanceAccountRefusal::NoKeyPairNothingAsked.carried())?;
        let hosts = hosts_of(inputs).map_err(|refusal| refusal.to_string().into());
        // The rows' client ids are read with the link id the start places this account's orders under.
        let link_id = attribution_code_from(inputs.secrets, VENUE);
        crate::reconcile::venue_read::venue_read(&creds, hosts, kind, link_id)
    }
}

#[path = "adapter_tests.rs"]
#[cfg(test)]
mod adapter_tests;
