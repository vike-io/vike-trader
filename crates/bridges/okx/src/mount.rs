//! okx's VENUE MOUNT — the `vike_bridge_core::venue_mount::VenueMount` contract over the V5 SWAP
//! exec client and its reconcile client
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md).
//!
//! Moved, behaviour unchanged, from `vike-mount`: the `("okx", Some(c))` arm (now
//! [`OkxVenueMount::mount`]), its row of the legacy arming probe ([`OkxVenueMount::resolve`]), its
//! clock, book-identity and grid-source rows ([`OkxVenueMount::declaration`]), its clock read and
//! parse ([`OkxVenueMount::server_time_ms`]), its `AUTHED_READ_MARKETS` startup probe
//! ([`OkxVenueMount::credential_probe`]), its `build_recon_client` dispatch, and its fallback grid
//! and `OKX_FALLBACK_CTVAL` (now `FALLBACK_CTVAL`). okx was the last of the CEX trio, so
//! `vike-mount`'s `recon.rs` and `fallback.rs` left with it. COPIED from its legacy prefix, which
//! kept them until the last legacy arm went (the dukascopy port deleted it): the no-LIVE-trio
//! warning word for word, and the half-credential report that prefix made for this venue with the
//! two changes the section below argues.
//!
//! # The ceiling decides the network (D1)
//!
//! `live` means mainnet (docs/decisions/0095-venues-read-no-environment-and-live-means-mainnet.md):
//! a `live` ceiling reads the LIVE key trio and drops the `x-simulated-trading` header, every lower
//! ceiling reads DEMO and keeps it. Demo and mainnet SHARE one REST host (`crate::perp::REST`), so
//! on the REST side that header is the whole switch (the private WS has one URL per tier,
//! `crate::perp::ws_url`) — and the clock read, a public endpoint on the shared host, has no tier
//! to choose. A `live` ceiling with no LIVE trio stays PAPER and `resolve` says why
//! (`PaperCause::LiveCredentialsAbsent`): a mainnet host is never signed with demo keys.
//!
//! # Three credentials, and the one report that makes a missing third audible
//!
//! okx's signer sends the passphrase on every request (`vike_bridge_core::venue_passphrase`'s
//! `Required` row), so a store with key and secret but no passphrase loads NOTHING and the venue
//! stays paper — correctly, and invisibly. `mount` names the missing variable at `error!`, the one
//! site that makes it audible. The contract has no paper cause of its own for it: `resolve` answers
//! what `vike-mount`'s arming row answered for this state, i.e. no key set at the ceiling's tier.
//!
//! ⚠ **The report lives in `mount`, not in `resolve`, and it is about the MOUNTING account.**
//! `vike-mount`'s legacy prefix emitted it BEFORE the pre-connect budget refusal, and read the
//! DEFAULT account's key names whichever account it was mounting; so a labelled account with a
//! half trio was never reported under its own key name, and a labelled account with a complete
//! trio was reported for the default account's missing passphrase. The line reads the mounting
//! account's names now (`missing_required_passphrase_for_account`) and ends in the store verb that
//! writes the key, never a file path. The contract fold runs the budget refusal before
//! it calls `mount`, and that refusal reaches only an account that ARMS — one whose trio is
//! complete, which has no finding to report — so a half trio always resolves paper, skips the
//! refusal and reaches this line. `resolve` could carry the line, but
//! every arming projection calls `resolve` — the preflight's legs, the live-lock probe
//! (`vike_mount::armed_live_venues`), the arming table — so it would print once per projection.
//!
//! # Contracts, not base units
//!
//! OKX sizes in CONTRACTS of `ctVal` base units. The grid handed to the fold is converted to BASE
//! (`crate::perp::properties_in_base`), because `OrderRequest.qty` and everything the gate reasons
//! about is base; the exec and reconcile clients keep the contracts grid and take the `ctVal`
//! themselves.

use std::time::Duration;

use vike_bridge_core::credentials::{
    Credentials, Environment, attribution_code_from, load_credentials_for_account,
    load_credentials_from, missing_required_passphrase_for_account,
};
use vike_bridge_core::venue_mount::{
    BookIdentity, ClockAuth, ClockDecl, ClockRisk, CredentialProbe, DeclaredGridSource,
    ExecOutcome, LiveExec, MountInputs, MountOutcome, MountRequest, PaperCause, Resolution, Tier,
    VenueDeclaration, VenueMount, bounded_public_get, missing_time_field,
};
use vike_model::SymbolProperties;

use crate::perp::VENUE;

/// The symbol the startup credential probe scopes its reconcile client to — was `vike-mount`'s
/// `AUTHED_READ_MARKETS` row, mirroring `vike_tradehub::wired_markets::WIRED_MARKETS`. The balance read the probe
/// issues is account-wide; the symbol only scopes the client's order and position reads.
const PROBE_SYMBOL: &str = "BTC-USDT-SWAP";

/// FALLBACK OKX contract value (0.01 BTC/contract, BTC-USDT-SWAP's), used ONLY if the mount's
/// startup `instruments` fetch fails — the adapter fetches the real `ctVal` per instId. Was
/// `vike-mount`'s `OKX_FALLBACK_CTVAL`. ⚠ It is ONE instrument's value applied to whatever instId
/// was mounted, so a failed fetch scales every size by a constant belonging to a different
/// contract — this crate's `CLAUDE.md` carries what that costs. It also scopes the startup
/// credential probe's reconcile client, whose balance read never uses it.
const FALLBACK_CTVAL: f64 = 0.01;

/// The line a `live` ceiling with no LIVE key set earns — copied word for word from the
/// `CexCredChoice::MainnetNoCreds` `warn!` of `vike-mount`'s legacy prefix (which served the three
/// CEX venues and kept it until the last legacy arm went), and logged behind the `okx: ` prefix
/// that line rendered.
const NO_LIVE_CREDENTIALS: &str = "the ceiling is `live` (MAINNET) but no LIVE credentials are \
    present → staying PAPER (a mainnet host is never signed with demo keys; absent credentials \
    are the live gate)";

/// FALLBACK OKX BTC-USDT-SWAP grid (contracts), used ONLY if the OKX adapter's startup
/// `instruments` fetch fails. Moved from `vike-mount`'s `fallback.rs` (`okx_fallback_properties`).
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
fn bound_tier(mainnet: bool) -> Tier {
    if mainnet { Tier::Live } else { Tier::Demo }
}

/// okx's mount. `vike_tradehub::registry::REGISTRY` holds `&OkxVenueMount`.
pub struct OkxVenueMount;

impl OkxVenueMount {
    /// The key tier the ceiling selects (D1): LIVE under a `live` ceiling, DEMO otherwise.
    fn tier(inputs: &MountInputs<'_>) -> Environment {
        if inputs.live_permitted { Environment::Live } else { Environment::Demo }
    }

    /// The one gate `resolve` and `mount` share: THIS account's full trio at that tier.
    fn credentials(inputs: &MountInputs<'_>) -> Option<Credentials> {
        load_credentials_for_account(VENUE, Self::tier(inputs), inputs.account, inputs.secrets)
    }

    /// The half-credential finding for the MOUNTING account: the NAME of the passphrase variable
    /// it is missing while its key and secret are present, else `None`. The name is the account's
    /// own (`OKX_DEMO_API_PASSPHRASE__ALT` for a labelled one), because that is the variable the
    /// operator has to write — the default account's names would send them to edit the account
    /// that is working, or find nothing to say about the one that is not.
    fn missing_passphrase(inputs: &MountInputs<'_>) -> Option<String> {
        missing_required_passphrase_for_account(
            VENUE,
            Self::tier(inputs),
            inputs.account,
            inputs.secrets,
        )
    }
}

impl VenueMount for OkxVenueMount {
    fn venue(&self) -> &'static str {
        VENUE
    }

    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration {
            // The arm reads the NAMED account's trio (`load_credentials_for_account`).
            addresses_accounts: true,
            process_exclusive: None,
            // The resync supervisor pokes the reconcile driver after every reconnect's
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

    /// A full trio at the ceiling's tier arms that tier; key and secret without the passphrase are
    /// no trio. With none, a `live` ceiling answers `LiveCredentialsAbsent` — whatever the DEMO
    /// trio holds, since this arm never falls back to it — and a lower one `NoCredentials`.
    /// `held_below_live` is always `None`: DEMO is reached only under a ceiling below `live`.
    fn resolve(&self, inputs: &MountInputs<'_>) -> Resolution {
        match Self::credentials(inputs) {
            Some(_) => {
                Resolution::Armed { tier: bound_tier(inputs.live_permitted), held_below_live: None }
            }
            None if inputs.live_permitted => Resolution::Paper(PaperCause::LiveCredentialsAbsent),
            None => Resolution::Paper(PaperCause::NoCredentials),
        }
    }

    fn mount(&self, req: MountRequest<'_>) -> MountOutcome {
        // THE HALF-CREDENTIAL REPORT, from `vike-mount`'s legacy prefix, with two changes the
        // module doc argues: it reads the MOUNTING account's key names, and it ends in the store
        // verb that writes the key. `error!`, not `warn!`: credentials that EXIST and cannot be used are the class of an
        // unreadable store, not of an unconfigured one. Here and not in the loader, which
        // `vike_connections::credential_status` runs every frame; and here and not in `resolve`,
        // for the reason the module doc gives. The NAME is logged; no credential value ever is.
        if let Some(missing) = Self::missing_passphrase(&req.inputs) {
            tracing::error!(
                venue = VENUE,
                "{missing} is unset or blank, but {VENUE} REQUIRES an API passphrase — its key \
                 and secret alone cannot sign a single request. These credentials are UNUSABLE, \
                 so {VENUE} stays PAPER (absent credentials are the live gate). Write it with \
                 `vike-cli secrets set {missing}` to mount it live."
            );
        }
        // Absent credentials ARE the live gate: no trio at the ceiling's tier is paper, and a
        // `live` ceiling says so rather than falling back to the DEMO trio on a mainnet host.
        let Some(c) = Self::credentials(&req.inputs) else {
            if req.inputs.live_permitted {
                tracing::warn!(venue = VENUE, "{VENUE}: {NO_LIVE_CREDENTIALS}");
            }
            return MountOutcome::paper();
        };
        // Decision 0095: the ceiling IS the network for this venue, so the one verdict every
        // network selection below binds to is `live_permitted` — resolved ONCE here and threaded
        // into the grid pre-fetch, the reconcile client and the exec spawn; no spawned thread
        // re-reads anything for itself.
        let mainnet = req.inputs.live_permitted;
        // The announcement convention — `venue`, `account`, `tier`, and `⚠ REAL-MONEY: ` on a live
        // tier only — is held for every bridge by `crates/vike-ops/tests/venues/live_mount_line_gate.rs`.
        if mainnet {
            tracing::warn!(
                venue = VENUE,
                account = %req.inputs.account,
                tier = Tier::Live.as_str(),
                "⚠ REAL-MONEY: okx mounting on MAINNET with LIVE credentials (real funds)"
            );
        } else {
            tracing::warn!(
                venue = VENUE,
                account = %req.inputs.account,
                tier = Tier::Demo.as_str(),
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
        let recon = crate::recon_client::recon_client(&c, req.symbol, ct_val, mainnet);
        // Fee-attribution FD-broker code (unified cross-venue attribution, task 4), resolved ONCE
        // from the credential map and validated against okx's `AttributionMechanic`. Absent or
        // invalid degrades to `None` — the wire body then carries no `tag` key at all.
        let broker_code = attribution_code_from(req.inputs.secrets, VENUE);
        // ACCOUNT LEVERAGE, resolved ONCE here from the operator's `[risk]` budget; UNSET ⇒ the
        // historical 2.0. It is the operator's REQUEST — the exec thread clamps it to the
        // instrument's `lever` (its published max), read off the `public/instruments` response it
        // already fetches for `ct_val`.
        let leverage = crate::exec::leverage_for(req.risk_profile);
        MountOutcome {
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
                    )
                    .with_halt_path(req.inputs.process.halt_path.clone()),
                ),
                bound_tier: bound_tier(mainnet),
                grid,
                contract_size: None,
                margin_mode: None,
                leg_grids: Vec::new(),
            }),
            recon,
            identity: None,
        }
    }

    /// `/api/v5/public/time` on `crate::perp::REST`. Demo and mainnet share the host, so there is
    /// no tier to resolve.
    fn server_time_ms(&self, _inputs: &MountInputs<'_>, timeout: Duration) -> Result<i64, String> {
        parse_server_time(&bounded_public_get(
            VENUE,
            crate::perp::REST,
            crate::perp::PATH_TIME,
            timeout,
        )?)
    }

    /// Was `vike-mount`'s eager authed-read probe over its `AUTHED_READ_MARKETS` row: the DEFAULT
    /// account's trio at the ceiling's tier, and a reconcile client built PURELY with
    /// `FALLBACK_CTVAL` (the balance read never uses it; the real value comes from the mount's own
    /// pre-fetch). `vike-mount` reads the balance, then records which account the key is.
    fn credential_probe(&self, inputs: &MountInputs<'_>) -> Option<CredentialProbe> {
        let creds = load_credentials_from(VENUE, Self::tier(inputs), inputs.secrets)?;
        let client = crate::recon_client::recon_client(
            &creds,
            PROBE_SYMBOL,
            FALLBACK_CTVAL,
            inputs.live_permitted,
        )?;
        Some(CredentialProbe::RecordsIdentity {
            client,
            bound_tier: bound_tier(inputs.live_permitted),
        })
    }
}

#[path = "mount_tests.rs"]
#[cfg(test)]
mod mount_tests;
