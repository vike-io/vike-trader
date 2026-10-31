//! okx's VENUE ADAPTER — the `vike_bridge_core::venue_adapter::VenueAdapter` contract over the V5 SWAP
//! exec client and its reconcile client
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md).
//!
//! Moved, behaviour unchanged, from `vike-tradehub-core`: the `("okx", Some(c))` arm (now
//! [`OkxVenueAdapter::start`]), its row of the legacy arming probe ([`OkxVenueAdapter::resolve`]), its
//! clock, book-identity and grid-source rows ([`OkxVenueAdapter::declaration`]), its clock read and
//! parse ([`OkxVenueAdapter::server_time_ms`]), its `AUTHED_READ_MARKETS` startup probe
//! ([`OkxVenueAdapter::credential_probe`]), its `build_recon_client` dispatch, and its fallback grid
//! and `OKX_FALLBACK_CTVAL` (now `FALLBACK_CTVAL`). okx was the last of the CEX trio, so
//! `vike-tradehub-core`'s `recon.rs` and `fallback.rs` left with it. COPIED from its legacy prefix, which
//! kept them until the last legacy arm went (the dukascopy port deleted it): the no-LIVE-trio
//! warning word for word, and the half-credential report that prefix made for this venue with the
//! two changes the section below argues.
//!
//! # The account's tier decides the network (D1)
//!
//! `live` means mainnet (docs/decisions/0095-venues-read-no-environment-and-live-means-mainnet.md):
//! a `live` account drops the `x-simulated-trading` header, every lower tier keeps it. Demo and
//! mainnet SHARE one REST host (`crate::perp::REST`), so on the REST side that header is the whole
//! switch (the private WS has one URL per tier, `crate::perp::ws_url`) — and the clock read, a
//! public endpoint on the shared host, has no tier to choose. The key trio is the started
//! ACCOUNT's (`AdapterInputs::credentials`, its `API_KEY` + `API_SECRET` + `API_PASSPHRASE` fields —
//! decision 0156 item 6): the account has ONE tier, so its keys are that tier's. A `live` account
//! with no complete trio does NOT RUN and `resolve` says why (`PaperCause::LiveCredentialsAbsent`).
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
//! (`crate::perp::properties_in_base`), because `OrderRequest.qty` and everything the gate reasons
//! about is base; the exec and reconcile clients keep the contracts grid and take the `ctVal`
//! themselves.

use std::time::Duration;

use vike_bridge_core::leverage_read::LeverageWriteError;
use vike_bridge_core::venue_adapter::{
    AdapterInputs, AdapterOutcome, AdapterRequest, BookIdentity, ClockAuth, ClockDecl, ClockRisk,
    CredentialProbe, DeclaredGridSource, ExecOutcome, LiveExec, PaperCause, Resolution,
    VenueAdapter, VenueDeclaration, bounded_public_get, missing_time_field,
};
use vike_model::AccountTier;
use vike_model::SymbolProperties;
use vike_secrets::credentials::{
    Credentials, api_credentials, attribution_code_from, missing_required_passphrase,
};

use crate::perp::VENUE;

/// The symbol the startup credential probe scopes its reconcile client to — was `vike-tradehub-core`'s
/// `AUTHED_READ_MARKETS` row.
/// It is this venue's row of `crates/vike-venues/src/venue_wiring.rs`'s `VENUE_WIRING` — the
/// symbol the daemon's default engine starts on — read at compile time, so the probe cannot drift
/// from the engine market and a row that loses its engine fails this crate's build. The balance read the probe
/// issues is account-wide; the symbol only scopes the client's order and position reads.
const PROBE_SYMBOL: &str = match vike_venues::venue_wiring::wired_symbol(VENUE) {
    Some(symbol) => symbol,
    None => {
        panic!("this venue's credential probe needs an engine row in vike_model's VENUE_WIRING")
    }
};

/// FALLBACK OKX contract value (0.01 BTC/contract, BTC-USDT-SWAP's), used ONLY if the start's
/// startup `instruments` fetch fails — the adapter fetches the real `ctVal` per instId. Was
/// `vike-tradehub-core`'s `OKX_FALLBACK_CTVAL`. ⚠ It is ONE instrument's value applied to whatever instId
/// was started, so a failed fetch scales every size by a constant belonging to a different
/// contract — this crate's `CLAUDE.md` carries what that costs. It also scopes the startup
/// credential probe's reconcile client, whose balance read never uses it.
const FALLBACK_CTVAL: f64 = 0.01;

/// The line a `live` account with no complete key trio earns — copied word for word from the
/// `CexCredChoice::MainnetNoCreds` `warn!` of `vike-tradehub-core`'s legacy prefix (which served the three
/// CEX venues and kept it until the last legacy arm went), and logged behind the `okx: ` prefix
/// that line rendered.
const NO_LIVE_CREDENTIALS: &str = "the account's tier is `live` (MAINNET) but no LIVE \
    credentials are present → staying PAPER (a mainnet host is never signed with demo keys; \
    absent credentials are the live gate)";

/// FALLBACK OKX BTC-USDT-SWAP grid (contracts), used ONLY if the OKX adapter's startup
/// `instruments` fetch fails. Moved from `vike-tradehub-core`'s `fallback.rs` (`okx_fallback_properties`).
fn fallback_properties() -> SymbolProperties {
    SymbolProperties {
        tick_size: 0.1,
        step_size: 0.01,
        min_qty: 0.01,
        max_qty: 100_000.0,
        min_notional: 1.0,
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

/// okx's envelope. ⚠ `data[0].ts` is epoch ms as a STRING inside an ARRAY — `as_i64()` reads
/// `None`, and an empty `data` array must not panic.
fn parse_server_time(body: &serde_json::Value) -> Result<i64, String> {
    body.get("data")
        .and_then(|d| d.get(0))
        .and_then(|row| row.get("ts"))
        .and_then(serde_json::Value::as_str)
        .and_then(|ts| ts.parse::<i64>().ok())
        .ok_or_else(|| missing_time_field("data[0].ts"))
}

/// The contract's tier for a resolved mainnet verdict.
fn bound_tier(mainnet: bool) -> AccountTier {
    if mainnet { AccountTier::Live } else { AccountTier::Demo }
}

/// **The network the account's tier selects** (D1): mainnet for [`AccountTier::Live`], the demo
/// network for [`AccountTier::Demo`]. [`AccountTier::Paper`] never reaches a bridge (`vike-tradehub-core`
/// returns before consulting one) and reads as the not-live branch, as `live_permitted == false`
/// did before decision 0129.
fn is_mainnet(tier: AccountTier) -> bool {
    match tier {
        AccountTier::Live => true,
        AccountTier::Demo | AccountTier::Paper => false,
    }
}

/// okx's start. `vike_tradehub::registry::REGISTRY` holds `&OkxVenueAdapter`.
pub struct OkxVenueAdapter;

impl OkxVenueAdapter {
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
}

impl VenueAdapter for OkxVenueAdapter {
    fn venue(&self) -> &'static str {
        VENUE
    }

    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration {
            // The arm reads the NAMED account's trio (`AdapterInputs::credentials`).
            addresses_accounts: true,
            process_exclusive: None,
            // The resync supervisor fires the recon trigger after every reconnect's
            // event-replay settles (`crate::exec::OkxExecutionClient::spawn_with_recorder`).
            takes_recon_trigger: true,
            // `crate::exec`'s `fetch_okx_instrument`: one symbol-scoped blocking public
            // `instruments` pre-fetch, which also yields `ctVal`.
            grid_source: DeclaredGridSource::PerSymbolFetch,
            // Nothing in the STORE names the account. The authenticated call that does is
            // `crate::recon_client`'s `fetch_account_identity` — one signed
            // `GET /api/v5/account/config`, genuinely new (nothing else on that client reads it).
            book_identity: BookIdentity::Undeterminable {
                why: "the store holds an api key/secret/passphrase trio and no account identifier; \
                      which sub-account the key belongs to is answerable only by an authenticated \
                      call",
            },
            clock: ClockDecl::Wired {
                endpoint: "GET /api/v5/public/time (public)",
                auth: ClockAuth::Public,
                risk: ClockRisk::SignedTimestamp,
            },
        }
    }

    /// The account's full trio arms the account's tier; key and secret without the passphrase are
    /// no trio. With none, a `live` tier answers `LiveCredentialsAbsent` and a lower one
    /// `NoCredentials`. `held_below_live` is always `None`: DEMO is reached only under a tier below
    /// `live`.
    fn resolve(&self, inputs: &AdapterInputs<'_>) -> Resolution {
        match Self::credentials(inputs) {
            Some(_) => Resolution::Armed {
                tier: bound_tier(is_mainnet(inputs.tier)),
                held_below_live: None,
            },
            None if is_mainnet(inputs.tier) => Resolution::Paper(PaperCause::LiveCredentialsAbsent),
            None => Resolution::Paper(PaperCause::NoCredentials),
        }
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
                 so {VENUE} stays PAPER (absent credentials are the live gate). Write it with \
                 `vike-cli secrets set {missing}` to mount it live."
            );
        }
        // Absent credentials ARE the live gate: an account with no complete trio is paper, and a
        // `live` one says so.
        let Some(c) = Self::credentials(&req.inputs) else {
            if is_mainnet(req.inputs.tier) {
                tracing::warn!(venue = VENUE, "{VENUE}: {NO_LIVE_CREDENTIALS}");
            }
            return AdapterOutcome::paper();
        };
        // Decision 0095: the tier IS the network for this venue, so the one verdict every
        // network selection below binds to is `is_mainnet(tier)` — resolved ONCE here and threaded
        // into the grid pre-fetch, the reconcile client and the exec spawn; no spawned thread
        // re-reads anything for itself.
        let mainnet = is_mainnet(req.inputs.tier);
        // The announcement convention — `venue`, `account`, `tier`, and `⚠ REAL-MONEY: ` on a live
        // tier only — is held for every bridge by `crates/vike-ops/tests/venues/live_mount_line_gate.rs`.
        if mainnet {
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
        let (grid, ct_val) = match crate::exec::fetch_okx_instrument(req.symbol, mainnet) {
            Some((f, ct_val)) => (Some(crate::perp::properties_in_base(&f, ct_val)), ct_val),
            None => (None, FALLBACK_CTVAL),
        };
        // Reconcile handle (audit A1 item 4): a FRESH stateless-HMAC REST client dedicated to
        // reconcile reads, built from the SAME `c` before it moves into `spawn_with_recorder`
        // below (the factory only borrows it). Pure to construct, so — as in the legacy arm — it is
        // not gated on `req.recon_enabled`.
        //
        // The admitted-instrument list is ONE `Arc`: this client is told each admission
        // (`ReconClient::admit_symbol`), and the funding poll of the exec spawn below reads the same list,
        // so an instrument the engine admits is also one whose funding bills fold.
        let admitted = std::sync::Arc::<vike_exec::recon::AdmittedSymbols>::default();
        let recon = crate::recon_client::recon_client_sharing(
            &c,
            req.symbol,
            ct_val,
            mainnet,
            admitted.clone(),
        );
        // Fee-attribution FD-broker code (unified cross-venue attribution, task 4), resolved ONCE
        // from the credential map and validated against okx's `AttributionMechanic`. Absent or
        // invalid degrades to `None` — the wire body then carries no `tag` key at all.
        let broker_code = attribution_code_from(req.inputs.secrets, VENUE);
        // ACCOUNT LEVERAGE, resolved ONCE here from the operator's `[risk]` budget; UNSET ⇒ the
        // historical 2.0. It is the operator's REQUEST — the exec thread clamps it to the
        // instrument's `lever` (its published max), read off the `public/instruments` response it
        // already fetches for `ct_val`.
        let leverage = crate::exec::leverage_for(req.risk_profile);
        AdapterOutcome {
            exec: ExecOutcome::Live(LiveExec {
                client: Box::new(
                    crate::exec::OkxExecutionClient::spawn_with_recorder(
                        c,
                        req.symbol.to_string(),
                        fallback_properties(),
                        FALLBACK_CTVAL,
                        req.events.clone(),
                        req.properties_rec,
                        req.recon_trigger,
                        broker_code,
                        mainnet,
                        leverage,
                        admitted,
                    )
                    .with_halt_path(req.inputs.process.halt_path.clone()),
                ),
                bound_tier: bound_tier(mainnet),
                grid,
                contract_value: None,
                margin_mode: None,
                leg_grids: Vec::new(),
            }),
            recon,
            identity: None,
        }
    }

    /// `/api/v5/public/time` on `crate::perp::REST`. Demo and mainnet share the host, so there is
    /// no tier to resolve.
    fn server_time_ms(
        &self,
        _inputs: &AdapterInputs<'_>,
        timeout: Duration,
    ) -> Result<i64, String> {
        parse_server_time(&bounded_public_get(
            VENUE,
            crate::perp::REST,
            crate::perp::PATH_TIME,
            timeout,
        )?)
    }

    /// Was `vike-tradehub-core`'s eager authed-read probe over its `AUTHED_READ_MARKETS` row: the probed
    /// account's trio, and a reconcile client built PURELY with `FALLBACK_CTVAL` (the balance read
    /// never uses it; the real value comes from the start's own pre-fetch). `vike-tradehub-core` reads the
    /// balance, then records which account the key is.
    fn credential_probe(&self, inputs: &AdapterInputs<'_>) -> Option<CredentialProbe> {
        let creds = Self::credentials(inputs)?;
        let client = crate::recon_client::recon_client(
            &creds,
            PROBE_SYMBOL,
            FALLBACK_CTVAL,
            is_mainnet(inputs.tier),
        )?;
        Some(CredentialProbe::RecordsIdentity {
            client,
            bound_tier: bound_tier(is_mainnet(inputs.tier)),
        })
    }

    /// The account's leverage for an instrument the engine was not mounted on: the exec half's own
    /// set-leverage ([`crate::exec::write_symbol_leverage`]) with THIS account's key trio, under the
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
        crate::exec::write_symbol_leverage(
            &creds,
            crate::perp::simulated(is_mainnet(inputs.tier)),
            symbol,
            leverage,
        )
    }

    /// The account's position mode put right BEFORE the gatewayhub's probe reads it: the start's own probe
    /// ([`crate::exec::heal_symbol_modes`]) with THIS account's key trio, under the simulated-trading
    /// header its tier selects. No complete key trio, no call.
    fn heal_modes(&self, inputs: &AdapterInputs<'_>, symbol: &str) -> Result<(), String> {
        let creds = Self::credentials(inputs).ok_or_else(|| {
            "the account holds no complete okx key set (key, secret, passphrase)".to_string()
        })?;
        crate::exec::heal_symbol_modes(
            &creds,
            crate::perp::simulated(is_mainnet(inputs.tier)),
            symbol,
        )
    }
}

#[path = "adapter_tests.rs"]
#[cfg(test)]
mod adapter_tests;
