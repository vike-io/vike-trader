//! okx's VENUE ADAPTER — the `vike_bridge_core::venue_adapter::VenueAdapter` contract over the V5 SWAP
//! exec client and its reconcile client
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md).
//!
//! Moved, behaviour unchanged, from `vike-tradehub-core`: the `("okx", Some(c))` arm (now
//! [`VenueAdapterOkx::start`]), its row of the legacy arming probe ([`VenueAdapterOkx::resolve`]), its
//! clock, book-identity and grid-source rows ([`VenueAdapterOkx::declaration`]), its clock read and
//! parse ([`VenueAdapterOkx::server_time_ms`]), its `AUTHED_READ_MARKETS` startup probe
//! ([`VenueAdapterOkx::credential_probe`]) and its `build_recon_client` dispatch. Its fallback grid and
//! `OKX_FALLBACK_CTVAL` are gone (owner, 2026-11-13): an instrument whose row has not loaded is refused in
//! words, and its reconcile client reads the contract size itself. okx was the last of the CEX trio, so
//! `vike-tradehub-core`'s `recon.rs` and `fallback.rs` left with it. COPIED from its legacy prefix, which
//! kept them until the last legacy arm went (the dukascopy port deleted it): the no-LIVE-trio
//! warning word for word, and the half-credential report that prefix made for this venue with the
//! two changes the section below argues.
//!
//! # The account's tier decides the network (D1)
//!
//! `live` means mainnet (docs/decisions/0095-venues-read-no-environment-and-live-means-mainnet.md):
//! a `live` account drops the `x-simulated-trading` header, every lower tier keeps it. Demo and
//! mainnet SHARE one REST host (the okx seed rows of `vike_venues::venue_urls::VENUE_URL_SEED`), so
//! on the REST side that header is the whole switch (the private sockets have one address per type).
//! Every address is the account's `venue_url` row at its type, resolved ONCE per call into
//! `crate::execution::rest::OkxNet` (`OkxNet::resolve` over `AdapterInputs::settings`): a refused lookup is an
//! account that does NOT RUN, with the refusal's words. The clock read is the account's own request,
//! on its own REST base. The key trio is the started
//! ACCOUNT's (`AdapterInputs::credentials`, its `API_KEY` + `API_SECRET` + `API_PASSPHRASE` fields —
//! decision 0156 item 6): the account has ONE tier, so its keys are that tier's. A `live` account
//! with no complete trio does NOT RUN and `resolve` says why (`ArmingNotRunningCause::LiveCredentialsAbsent`).
//!
//! # Three credentials, and the one report that makes a missing third audible
//!
//! okx's signer sends the passphrase on every request (`vike_secrets::venue_passphrase`'s
//! `Required` row), so an account with key and secret but no passphrase loads NOTHING and the venue
//! does not run — correctly, and invisibly. `start` names the missing FIELD at `error!`, the one
//! site that makes it audible. The contract has no paper cause of its own for it: `resolve` answers
//! what `vike-tradehub-core`'s arming row answered for this state, i.e. no key set at the account's tier.
//!
//! ⚠ **The report lives in `start`, not in `resolve`, and it is about the STARTING account.**
//! `vike-tradehub-core`'s legacy prefix emitted it BEFORE the pre-connect budget refusal, and read the
//! DEFAULT account's key names whichever account it was starting. The line reads the starting
//! account's own fields now (`vike_secrets::credentials::missing_required_passphrase` over
//! `AdapterInputs::credentials`) and ends in the store verb that writes the key, never a file path.
//! The contract fold runs the budget refusal before
//! it calls `start`, and that refusal reaches only an account that ARMS — one whose trio is
//! complete, which has no finding to report — so a half trio always resolves paper, skips the
//! refusal and reaches this line. `resolve` could carry the line, but
//! every arming projection calls `resolve` — the preflight's legs, the live-lock probe
//! (`vike_tradehub_core::armed_live_venues`), the arming table — so it would print once per projection.
//!
//! # Contracts, not base units
//!
//! OKX sizes in CONTRACTS of `ctVal` base units. The grid handed to the fold is converted to BASE
//! (`crate::execution::rest::properties_in_base`), because `OrderRequest.qty` and everything the gate reasons
//! about is base; the exec and reconcile clients keep the contracts grid and take the `ctVal`
//! themselves. A COIN-MARGINED mount (`BTC-USD-SWAP`, decision 0258's okx amendment) is the exception:
//! its quantity IS the contract count (each worth `ctVal x ctMult` US dollars, the `Inverse` contract value
//! the start reports), so the grid stays in contracts and every client's factor is 1.0 (`mount_parts`).

use std::time::Duration;

use vike_bridge_core::leverage_read::LeverageWriteError;
use vike_bridge_core::venue_adapter::{
    AdapterInputs, AdapterOutcome, AdapterRequest, ArmingResolution, BookIdentityRule,
    ClockAuthentication, ClockDeclaration, CredentialProbe, VenueAdapter, VenueAdapterDeclaration,
    VenueAdapterExecutionOutcome, VenueAdapterExecutionRunning, bound_tier, bounded_public_get,
    missing_time_field, report_address_refused, report_live_credentials_absent,
    resolve_one_key_set,
};
use vike_model::AccountTier;
use vike_model::{ClockRisk, GridSource, SymbolProperties, VenueNetwork};
use vike_secrets::credentials::{
    Credentials, api_credentials, attribution_code_from, missing_required_passphrase,
};

use crate::execution::rest::{OkxNet, VENUE};

/// **What the start reports for the mounted instrument's row**: the grid the fold sizes on, the factor the
/// reconcile client scales a contract count by, and the contract value. A LINEAR row: the grid in BASE units
/// and `ctVal`, no value (unchanged). A COIN-MARGINED row (`crate::execution::mount_money`): the venue's contracts
/// grid as listed and the factor 1.0 (the quantity IS the contract count, each worth `ctVal x ctMult` US
/// dollars), and `Inverse` of that worth. No row: for a linear name no grid (the permissive default) and no
/// factor, which the reconcile client then reads itself, never guessed (owner, 2026-11-13); a refusal for a
/// coin-margined one.
#[expect(
    clippy::type_complexity,
    reason = "the start's three answers, each optional, read once at its one caller"
)]
fn mount_parts(
    symbol: &str,
    row: Option<crate::execution::rest::OkxInstrument>,
) -> Result<(Option<SymbolProperties>, Option<f64>, Option<vike_model::ContractValue>), String> {
    let money = crate::execution::mount_money(symbol, row.as_ref())?;
    Ok(match (row, money) {
        (Some(row), Some((value, _coin))) => (Some(row.properties), Some(1.0), Some(value)),
        (Some(row), None) => (
            Some(crate::execution::rest::properties_in_base(&row.properties, row.ct_val)),
            Some(row.ct_val),
            None,
        ),
        (None, _) => (None, None, None),
    })
}

/// okx's envelope's stamp ([`crate::transport::client::server_time`], the one reading of it, which the signing clock also
/// takes).
fn parse_server_time(body: &serde_json::Value) -> Result<i64, String> {
    crate::transport::client::server_time(body).ok_or_else(|| missing_time_field("data[0].ts"))
}

/// okx's start. `vike_tradehub::registry::REGISTRY` holds `&VenueAdapterOkx`.
pub struct VenueAdapterOkx;

impl VenueAdapterOkx {
    /// The one gate `resolve`, `start` and the startup probe share: THIS account's full trio — its
    /// `API_KEY`, `API_SECRET` and `API_PASSPHRASE` fields
    /// (`vike_secrets::credentials::api_credentials`), which are its tier's because the account
    /// has one tier.
    fn credentials(inputs: &AdapterInputs<'_>) -> Option<Credentials> {
        api_credentials(&inputs.credentials, VENUE)
    }

    /// The half-credential finding for the STARTING account: the FIELD it is missing
    /// (`API_PASSPHRASE`) while its key and secret are present, else `None` — read from the
    /// started account's own credentials, so it never speaks about another account's.
    fn missing_passphrase(inputs: &AdapterInputs<'_>) -> Option<&'static str> {
        missing_required_passphrase(&inputs.credentials, VENUE)
    }

    /// The account's addresses at its type ([`OkxNet::resolve`] over its `venue_url` rows); `Err` is
    /// the refusal's words, the reason the account does NOT RUN.
    fn net(inputs: &AdapterInputs<'_>) -> Result<OkxNet, String> {
        OkxNet::resolve(inputs.settings.urls(), inputs.tier).map_err(|refusal| refusal.to_string())
    }
}

impl VenueAdapter for VenueAdapterOkx {
    fn venue(&self) -> &'static str {
        VENUE
    }

    fn declaration(&self) -> VenueAdapterDeclaration {
        VenueAdapterDeclaration {
            // The arm reads the NAMED account's trio (`AdapterInputs::credentials`).
            addresses_accounts: true,
            process_exclusive: None,
            // The resync supervisor fires the recon trigger after every reconnect's
            // event-replay settles (`crate::execution::spawn_with_recorder`).
            takes_reconcile_trigger: true,
            // `crate::execution`'s `fetch_okx_instrument`: one symbol-scoped blocking public
            // `instruments` pre-fetch, which also yields `ctVal`.
            grid_source: GridSource::PerSymbolFetch,
            // Nothing in the STORE names the account. The authenticated call that does is
            // `crate::reconcile`'s `fetch_account_identity` — one signed
            // `GET /api/v5/account/config`, genuinely new (nothing else on that client reads it).
            book_identity: BookIdentityRule::Undeterminable {
                why: "the store holds an api key/secret/passphrase trio and no account identifier; \
                      which sub-account the key belongs to is answerable only by an authenticated \
                      call",
            },
            clock: ClockDeclaration::Wired {
                endpoint: "GET /api/v5/public/time (public)",
                auth: ClockAuthentication::Public,
                risk: ClockRisk::SignedTimestamp,
            },
        }
    }

    /// The account's full trio arms the account's tier; key and secret without the passphrase are
    /// no trio. With none, a `live` tier answers `LiveCredentialsAbsent` and a lower one
    /// `NoCredentials`. `held_below_live` is always `None`: DEMO is reached only under a tier below
    /// `live`.
    fn resolve(&self, inputs: &AdapterInputs<'_>) -> ArmingResolution {
        resolve_one_key_set(
            Self::credentials(inputs).is_some(),
            VenueNetwork::for_tier(inputs.tier),
        )
    }

    fn start(&self, req: AdapterRequest<'_>) -> AdapterOutcome {
        // THE HALF-CREDENTIAL REPORT, from `vike-tradehub-core`'s legacy prefix, with two changes the
        // module doc argues: it reads the STARTING account's own fields, and it ends in the store
        // verb that writes the key. `error!`, not `warn!`: credentials that EXIST and cannot be used are the class of an
        // unreadable store, not of an unconfigured one. Here and not in the loader, which
        // `vike_gateway_panel::credential_status` runs every frame; and here and not in `resolve`,
        // for the reason the module doc gives. The FIELD is logged; no credential value ever is.
        if let Some(missing) = Self::missing_passphrase(&req.inputs) {
            tracing::error!(
                venue = VENUE,
                "{missing} is unset or blank, but {VENUE} REQUIRES an API passphrase — its key \
                 and secret alone cannot sign a single request. These credentials are UNUSABLE, \
                 so {VENUE} is NOT RUNNING (absent credentials are the live gate). Write it with \
                 `vike-cli secrets set {missing}` to mount it live."
            );
        }
        // Absent credentials ARE the live gate: an account with no complete trio is paper, and a
        // `live` one says so.
        let Some(c) = Self::credentials(&req.inputs) else {
            report_live_credentials_absent(VENUE, req.inputs.account, req.inputs.tier);
            return AdapterOutcome::paper();
        };
        // Decision 0095: the tier IS the network for this venue, so the one value every network
        // selection below binds to is the account's addresses at its type (`net`, its network is
        // `VenueNetwork::for_tier(tier)`) — resolved ONCE here and threaded into the grid pre-fetch,
        // the reconcile client and the exec spawn; no spawned thread re-reads anything for itself.
        // A refused lookup dials nothing: the account is NOT RUNNING, with the row it lacks.
        let net = match Self::net(&req.inputs) {
            Ok(net) => net,
            Err(why) => {
                report_address_refused(VENUE, req.inputs.account, why);
                return AdapterOutcome::paper();
            }
        };
        let bound = bound_tier(VenueNetwork::for_tier(req.inputs.tier));
        // The announcement convention — `venue`, `account`, `tier`, and `⚠ REAL-MONEY: ` on a live
        // tier only — is held for every bridge by `crates/vike-ops/tests/venues/live_mount_line_gate.rs`.
        if bound == AccountTier::Live {
            tracing::warn!(
                venue = VENUE,
                account = %req.inputs.account,
                tier = AccountTier::Live.as_str(),
                "⚠ REAL-MONEY: okx mounting on MAINNET with LIVE credentials (real funds)"
            );
        } else {
            tracing::warn!(
                venue = VENUE,
                account = %req.inputs.account,
                tier = AccountTier::Demo.as_str(),
                "okx: DEMO credentials present → LIVE exec client (real demo orders)"
            );
        }
        // PIT filter recording (opt-in): `req.properties_rec` is the caller-built recorder handle —
        // `None` unless the binary armed one, so the disabled path is byte-identical. It moves into
        // the exec spawn below.
        //
        // Live RiskGate from the venue's REAL grid (PR-2b): one blocking pre-fetch (keyless, a
        // duplicate of the adapter's own in-thread fetch — acceptable, startup-only). On failure
        // the permissive default stands (byte-identical to pre-PR-2b behavior). The SAME fetch also
        // yields `ct_val` (contracts -> base), reused for the reconcile client instead of fetching
        // it a second time.
        //
        // ⚠ BASE units, not the raw grid. OKX's `lotSz`/`minSz`/`maxMktSz` count CONTRACTS — that
        // is what `OkxInstrument::to_contracts` floors a base qty on, and what every wire-side
        // consumer needs. But `OrderRequest.qty` is BASE, and so is everything the gate reasons
        // about, so handing it the contracts grid makes it floor base quantities on a step
        // `ct_val`-times too coarse: for BTC-USDT-SWAP (`ct_val` 0.01) that is 100x, and a
        // legitimate 0.015 BTC order floors to 0.01 while a size the venue accepts is refused
        // outright. The conversion lives in ONE place next to `to_contracts` so the unit law is
        // not re-derived here.
        //
        // The same row says what a unit of the quantity is WORTH (decisions 0170, 0258): a coin-margined
        // mount reports `Inverse(ctVal x ctMult)` and its quantity is the contract count (`mount_parts`).
        // ⚠ NO ROW, NO MOUNT for a name that reads as coin-margined: its kind is never defaulted to linear,
        // so the account does NOT RUN, with the reason (the supervisor retries), as bybit's start does.
        let row = crate::execution::fetch_okx_instrument_row(req.symbol, &net).ok();
        let (grid, ct_val, contract_value) = match mount_parts(req.symbol, row) {
            Ok(parts) => parts,
            Err(why) => {
                tracing::error!(
                    venue = VENUE,
                    account = %req.inputs.account,
                    symbol = req.symbol,
                    "okx: REFUSED — {why}; the account is NOT RUNNING until the row can be read"
                );
                return AdapterOutcome::paper();
            }
        };
        // Reconcile handle (audit A1 item 4): a FRESH stateless-HMAC REST client dedicated to
        // reconcile reads, built from the SAME `c` before it moves into `spawn_with_recorder`
        // below (the factory only borrows it). Pure to construct, so — as in the legacy arm — it is
        // not gated on `req.recon_enabled`.
        //
        // The admitted-instrument list is ONE `Arc`: this client is told each admission
        // (`ReconcileClient::admit_symbol`), and the funding poll of the exec spawn below reads the same list,
        // so an instrument the engine admits is also one whose funding bills fold.
        let admitted = std::sync::Arc::<
            vike_bridge_core::reconcile_admitted::ReconcileAdmittedSymbols,
        >::default();
        // ONE signing clock for this start (owner ruling B-4): the reconcile client, every client of the exec spawn
        // and both private sockets' logins stamp the venue's time, read by the first of them to sign.
        let clock = vike_bridge_core::signer::VenueClock::default();
        let recon = crate::reconcile::reconcile_client_sharing(
            &c,
            req.symbol,
            ct_val,
            &net,
            admitted.clone(),
            &clock,
            &req.inputs.account.to_string(),
        );
        // Fee-attribution FD-broker code (unified cross-venue attribution, task 4), resolved ONCE
        // from the credential map and validated against okx's `AttributionMechanic`. Absent or
        // invalid degrades to `None` — the wire body then carries no `tag` key at all.
        let broker_code = attribution_code_from(req.inputs.secrets, VENUE);
        // ACCOUNT LEVERAGE, resolved ONCE here from the operator's `[risk]` budget; UNSET ⇒ the
        // historical 2.0. It is the operator's REQUEST — the exec thread clamps it to the
        // instrument's `lever` (its published max), read off the `public/instruments` response it
        // already fetches for `ct_val`.
        let leverage = crate::execution::leverage_for(req.risk_profile);
        AdapterOutcome {
            exec: VenueAdapterExecutionOutcome::Running(VenueAdapterExecutionRunning {
                client: Box::new(
                    crate::execution::spawn_with_recorder(
                        c,
                        req.symbol.to_string(),
                        req.events.clone(),
                        req.properties_rec,
                        req.reconcile_trigger,
                        broker_code,
                        net,
                        leverage,
                        admitted,
                        clock,
                    )
                    .with_halt_path(req.inputs.process.halt_path.clone()),
                ),
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

    /// `/api/v5/public/time` on the account's own REST base ([`OkxNet::rest`] at its type; demo and
    /// mainnet share the host, so both read one clock). A refused lookup is the leg's error.
    fn server_time_ms(&self, inputs: &AdapterInputs<'_>, timeout: Duration) -> Result<i64, String> {
        let net = Self::net(inputs)?;
        parse_server_time(&bounded_public_get(
            VENUE,
            &net.rest,
            crate::execution::rest::PATH_TIME,
            timeout,
        )?)
    }

    /// Was `vike-tradehub-core`'s eager authed-read probe over its `AUTHED_READ_MARKETS` row: the probed
    /// account's trio, and a reconcile client built with NO contract size (the balance read never uses one;
    /// a read that scales a size asks the venue for it first). `vike-tradehub-core` reads the
    /// balance, then records which account the key is. An account whose addresses are refused gets a
    /// probe that FAILS with the refusal's words (never no probe: silence where the leg exists to fail).
    fn credential_probe(&self, inputs: &AdapterInputs<'_>) -> Option<CredentialProbe> {
        let creds = Self::credentials(inputs)?;
        let net = match Self::net(inputs) {
            Ok(net) => net,
            Err(why) => return Some(CredentialProbe::failing(why)),
        };
        // No symbol (owner, 2026-11-13: none is compiled in, and the venue needs none here): the probe reads the
        // account-wide balance alone, which no instrument scopes.
        let client = crate::reconcile::recon_client(&creds, "", None, &net)?;
        Some(CredentialProbe::RecordsIdentity {
            client,
            bound_tier: bound_tier(VenueNetwork::for_tier(inputs.tier)),
        })
    }

    /// The account's leverage for an instrument the engine was not mounted on: the exec half's own
    /// set-leverage ([`crate::execution::write_symbol_leverage`]) with THIS account's key trio, under the
    /// simulated-trading header its tier selects. No complete key trio, no call.
    fn set_symbol_leverage(
        &self,
        inputs: &AdapterInputs<'_>,
        symbol: &str,
        leverage: f64,
    ) -> Result<(), LeverageWriteError> {
        let creds = Self::credentials(inputs).ok_or_else(|| {
            LeverageWriteError::Refused(
                "the account holds no complete okx key set (key, secret, passphrase)".to_string(),
            )
        })?;
        let net = Self::net(inputs).map_err(LeverageWriteError::Refused)?;
        crate::execution::write_symbol_leverage(&creds, &net, symbol, leverage)
    }

    /// The account's position mode put right BEFORE the gatewayhub's probe reads it: the start's own probe
    /// ([`crate::execution::heal_symbol_modes`]) with THIS account's key trio, under the simulated-trading
    /// header its tier selects. No complete key trio, no call.
    fn heal_modes(&self, inputs: &AdapterInputs<'_>, symbol: &str) -> Result<(), String> {
        let creds = Self::credentials(inputs).ok_or_else(|| {
            "the account holds no complete okx key set (key, secret, passphrase)".to_string()
        })?;
        crate::execution::heal_symbol_modes(&creds, &Self::net(inputs)?, symbol)
    }

    /// An operator's READ of the account (`crate::reconcile::venue_read`): every resting order and algo order of the account,
    /// with THIS account's key trio, under the simulated-trading header its tier selects. It writes nothing. No
    /// complete key trio, nothing asked; the balance and the positions are not offered.
    fn venue_read(
        &self,
        inputs: &AdapterInputs<'_>,
        kind: vike_model::VenueReadKind,
    ) -> Result<vike_model::VenueRead, String> {
        let creds = Self::credentials(inputs).ok_or_else(|| {
            "the account holds no complete okx key set (key, secret, passphrase); nothing was asked"
                .to_string()
        })?;
        crate::reconcile::venue_read::venue_read(&creds, Self::net(inputs), kind)
    }
}

#[path = "adapter_tests.rs"]
#[cfg(test)]
mod adapter_tests;
