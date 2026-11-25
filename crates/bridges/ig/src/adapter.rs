//! ig's VENUE ADAPTER — the `vike_bridge_core::venue_adapter::VenueAdapter` contract over the
//! session-auth REST exec client and its dedicated reconcile session
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md).
//!
//! Moved, behaviour unchanged, from `vike-tradehub-core`: the `("ig", _)` arm, its arming-probe row, its
//! book-identity row and its clock row WITH the read (the read `vike-tradehub-core` called `ig_time` is
//! [`VenueAdapterIg::server_time_ms`]).
//!
//! IG (IG Group) FX/CFD via session-auth REST. LIVE exec when an account below `live` holds its
//! `API_KEY` / `IDENTIFIER` / `PASSWORD` fields (`AdapterInputs::credentials`), else paper. An
//! EXEC-ONLY start: market data comes from a separate feed. `crate::execution::spawn_execution_client` is
//! INFALLIBLE at start time (an ExecutionClientActor + a background Lightstreamer stream that each self-gate
//! on their own login). The ONE read a start makes first is the market row of the mounted USER name's epic (its
//! kept row's, `crate::epic`; decision 0290), for what one unit of its size is worth (`crate::instrument`'s
//! `mount_contract_value`, the value an admitted epic is booked by): present creds and a readable row ⇒ live; a
//! name with no kept row ⇒ live with no value read and every order on that name refused (a start never needs a
//! row: IG's list is the operator's, written over a running account's session); a row that cannot be read ⇒
//! NOT RUNNING with the reason, by the failure's kind (`AdapterOutcome::failed` over
//! `crate::instrument::IgPropertiesError::kind`): a read a later try can pass (IG not reached, a timeout, a 5xx,
//! a rate limit) is `Unreachable` and the start is asked again by itself, a refusal (a declined login, an epic
//! IG says is no market) waits for the operator; absent creds ⇒ not running, silently.
//!
//! ⚠ **A LIVE ACCOUNT'S KEY SET IS NAMED, NOT IGNORED.** This arm has no live arm: it starts only
//! the DEMO gateway. An account has ONE tier and its credentials are that tier's (decision 0156
//! item 6), so a `live` account holding a complete trio holds a set this arm will not use: it
//! answers `ArmingNotRunningCause::LiveTierNotWired` rather than `NoCredentials` (the words of an empty
//! account), and `start` says so at `error!`
//! (`vike_bridge_core::venue_adapter::report_live_tier_not_wired`). No venue starts live that did not
//! before. What the arm says about the live tier is ONE function
//! ([`ArmingDemoOnly::report_live_tier`]), which also speaks for a labelled account that is never started
//! ([`VenueAdapter::report_not_running_account`]); an incomplete or empty account is silent.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_bridge_core::execution_client::order_client_id::OrderVenueIdRegistry;
use vike_bridge_core::venue_adapter::{
    AdapterInputs, AdapterOutcome, AdapterRequest, ArmingDemoOnly, ArmingResolution,
    BookIdentityRule, ClockAuthentication, ClockDeclaration, VenueAdapter, VenueAdapterDeclaration,
    VenueAdapterExecutionOutcome, VenueAdapterExecutionRunning, reconcile_if_enabled,
};
use vike_execution::recon::ReconcileClient;
use vike_execution::{EventSender, ExecutionClient};
use vike_model::{AccountTier, ClockRisk, ContractValue, GridSource};
use vike_venues::venue_urls::UrlRefusal;

use crate::config::{FIELDS, IgConfig, demo_only_arm, load_ig_config_from, pin_of};
use crate::instrument::IgPropertiesError;
use crate::reconcile::VENUE;

/// ig's start. `vike_tradehub::registry::REGISTRY` holds `&VenueAdapterIg`.
pub struct VenueAdapterIg;

impl VenueAdapterIg {
    /// The one decision `resolve`, `start` and `report_not_running_account` act on: this account's
    /// trio at this account's tier, on the venue's address rows (`crate::config`'s `demo_only_arm`).
    fn arm(inputs: &AdapterInputs<'_>) -> ArmingDemoOnly<Result<IgConfig, UrlRefusal>> {
        match demo_only_arm(inputs.tier, &inputs.credentials, inputs.settings.urls()) {
            // The row's book rides the config, so every login of this start (exec, stream, reconcile, the
            // feeds it spawns) puts its session on the account the operator named (`crate::account::pick`), and
            // so does the FX login gate's clock (`IgConfig::fx_clock`).
            ArmingDemoOnly::Demo(Ok(mut config)) => {
                config.account_pin = pin_of(inputs);
                config.fx_clock = inputs.fx_clock.cloned();
                ArmingDemoOnly::Demo(Ok(config))
            }
            other => other,
        }
    }
}

/// [`VenueAdapterIg::start`]'s body, with its three NETWORK steps taken as parameters: `mount_value`
/// reads what one unit of the mounted epic is worth (`crate::instrument`'s `mount_contract_value`),
/// `spawn` builds the exec client (whose ExecutionClientActor and Lightstreamer stream each log in from their own
/// thread; its third argument is the mounted name no value was read for) and `connect_reconcile` is the dedicated
/// reconcile login. `start` passes the real three; a test passes offline doubles, which is what lets
/// `adapter_tests.rs` read the fields THIS body sets — the bound tier and the contract value above all — and count
/// the logins it attempts.
fn start_with(
    req: AdapterRequest<'_>,
    mount_value: impl FnOnce(&IgConfig, &str) -> Result<ContractValue, IgPropertiesError>,
    spawn: impl FnOnce(IgConfig, EventSender, Option<String>) -> Box<dyn ExecutionClient + Send>,
    connect_reconcile: impl FnOnce(&IgConfig, &str) -> Option<Box<dyn ReconcileClient>>,
) -> AdapterOutcome {
    // No usable trio ⇒ paper. A LIVE account's complete trio is paper too, and the one thing said:
    // why the venue does not run. A present trio whose demo gateway row was refused is NOT RUNNING,
    // said in the refusal's words (they name the row), before anything logs in.
    let Some(config) = VenueAdapterIg::arm(&req.inputs).config_for_start(VENUE, req.inputs.account)
    else {
        return AdapterOutcome::paper();
    };
    // ⚠ NO MARKET ROW, NO VALUE, NO MOUNT. One unit of an epic's size is `contractSize` units of what it buys
    // (10,000 on the wired EUR/USD Mini), the figure an admitted epic is already booked by; booked at 1 the
    // engine would size every notional, margin and PnL 10,000 times small. So the mount reads it from the
    // same source before anything else logs in, and an account whose row cannot be read does not run: asked
    // again by itself when the read failed on something a later try can pass, else until the operator acts.
    //
    // The mounted symbol is a USER name (decision 0290) and the epic read is its KEPT row's (`crate::epic`). IG's list
    // is the operator's rows, added over a RUNNING ig account's session (`crate::operator_row`), so a start never
    // needs one: with no row the account runs, no value is read, and its exec client refuses every order on that
    // name until a start reads one (`unvalued`): nothing is booked at a unit of 1.
    let (contract_value, unvalued) = match crate::epic::epic_of(req.symbol) {
        Err(why) => {
            tracing::warn!(
                venue = VENUE,
                account = %req.inputs.account,
                symbol = req.symbol,
                "ig: {why}; what one unit of it is worth is not read, so this account runs and refuses every order \
                 on it until a start reads its row"
            );
            (None, Some(req.symbol.to_string()))
        }
        Ok(epic) => match mount_value(&config, &epic) {
            Ok(value) => (Some(value), None),
            Err(why) => {
                let kind = why.kind();
                tracing::error!(
                    venue = VENUE,
                    account = %req.inputs.account,
                    symbol = req.symbol,
                    %kind,
                    "ig: REFUSED — what one unit of {} ({epic}) is worth could not be read ({}), and a guess \
                     would size every order at the wrong notional; the account is NOT RUNNING {}",
                    req.symbol,
                    why.text(),
                    if kind.is_terminal() {
                        "until its market row can be read (this failure waits for the operator)"
                    } else {
                        "yet: its start is asked again by itself until the market row can be read"
                    }
                );
                return AdapterOutcome::failed(kind);
            }
        },
    };
    tracing::warn!(
        venue = VENUE,
        account = %req.inputs.account,
        tier = AccountTier::Demo.as_str(),
        "ig: DEMO credentials present → LIVE exec client (real demo orders)"
    );
    // Reconcile handle: `crates/bridges/ig/src/reconcile/mod.rs`'s `recon_client` logs in a FRESH
    // dedicated `IgSession`, isolated from BOTH the exec side's own session and its background
    // Lightstreamer login (IG permits concurrent sessions). Built from `&config` HERE, before `config`
    // moves into `spawn` below; the login is a blocking round trip at start time, and a refused one
    // (`None`) leaves the venue reconcile-inert with exec unaffected (one that never reached IG is made by
    // a later pass). Interval-only (no
    // `reconcile_trigger`). ORDER-COMPLETENESS: `/workingorders` is the CURRENT resting set, so a local
    // order absent from it is a genuine terminal (`OrphanLocalOrder`); the rollout exposure is
    // POSITION-side (`hybrid` auto-applies `PositionDrift`). ⚠ The order-client-id-keyed order paths DO apply to
    // IG, for the orders THIS process placed: `start` hands the reconcile client the exec client's
    // `order_client_id <-> dealId` map, so a live one is reported under its own order client id. Without that, every
    // working order paired an `UnknownOrder` with an `OrphanLocalOrder` (a report's order client id was always
    // `None`); an order from before a restart, or one whose `dealId` was never learned, still does.
    //
    // LAZY: with reconciliation off that dedicated login never happens.
    let recon =
        reconcile_if_enabled(req.reconcile_enabled, || connect_reconcile(&config, req.symbol));
    AdapterOutcome {
        exec: VenueAdapterExecutionOutcome::Running(VenueAdapterExecutionRunning {
            client: spawn(config, req.events.clone(), unvalued),
            // The tier the demo account's trio authenticates, which `vike-tradehub-core` records the
            // account against: the arm has no live tier.
            bound_tier: AccountTier::Demo,
            grid: None,
            contract_value,
            margin_mode: None,
            leg_grids: Vec::new(),
        }),
        reconcile: recon,
        identity: None,
    }
}

impl VenueAdapter for VenueAdapterIg {
    fn venue(&self) -> &'static str {
        VENUE
    }

    fn declaration(&self) -> VenueAdapterDeclaration {
        VenueAdapterDeclaration {
            addresses_accounts: true,
            process_exclusive: None,
            // An exec-only arm with NO grid pre-fetch: the engine symbol keeps
            // `RiskLimits::new()`'s permissive `None`s, so a leg inherits nothing.
            grid_source: GridSource::NoGrid,
            // `crates/bridges/ig/src/config.rs`'s `FIELDS`: the `IDENTIFIER` field is the LOGIN.
            // ⚠ It names the login rather than the dealing account, and IG can hold several
            // accounts behind one login — so this row is sound in the direction that matters (two
            // labels with one identifier ARE one login, and this daemon selects that login's
            // default account both times) and silent in the other (two different logins on one
            // underlying account cannot be seen from here). A warning that fires only on a
            // certainty is the contract; the missed case is a miss, not a false report.
            // ⚠ A row's RECORDED book wins over this rule (`book_of_account`), and for IG that is
            // the dealing-account id `IgConfig::account_pin` selects, not the login: a pinned and an
            // unpinned account on one login and one dealing account never pair — again a miss.
            book_identity: BookIdentityRule::Named {
                name_fields: &["IDENTIFIER"],
                evm_key_fields: &[],
            },
            // The one CLOCK read on the roster that sends a credential, and it is cheap: the API
            // key alone, no `POST /session` login, no session to expire. The key is guaranteed
            // present whenever this row can fire — the clock leg runs only for a venue the arming
            // projection accepts, and IG's live gate IS that key. ⚠ That projection is
            // CEILING-AWARE: an ig capped to `paper` is never read at all.
            clock: ClockDeclaration::Wired {
                endpoint: "GET /session/encryptionKey (X-IG-API-KEY only)",
                auth: ClockAuthentication::Credentialed,
                risk: ClockRisk::NoTimestamp,
            },
        }
    }

    /// A complete trio on an account below `live` arms DEMO (held by `ArmingDemoOnly`); a `live`
    /// account's complete trio is `LiveTierNotWired`; anything less is `NoCredentials`
    /// ([`ArmingDemoOnly::resolution`]).
    fn resolve(&self, inputs: &AdapterInputs<'_>) -> ArmingResolution {
        Self::arm(inputs).resolution()
    }

    fn start(&self, req: AdapterRequest<'_>) -> AdapterOutcome {
        // Read BEFORE `req` moves into `start_with`; the spawn closure hands it to the client.
        let halt_path = req.inputs.process.halt_path.clone();
        // ONE `order_client_id <-> dealId` map for this start, handed to BOTH the exec client (which fills it
        // at submit and at the confirm watchdog) and the reconcile client (which reads it to name a
        // live working order's order client id) — two maps would be the defect this replaces.
        let deal_ids = Arc::new(Mutex::new(OrderVenueIdRegistry::<String>::default()));
        let reconcile_ids = deal_ids.clone();
        // The account's path cell, which the gatewayhub's status verb reads (`crate::account::venue_link`).
        let link = crate::account::venue_link::session_link(req.inputs.account);
        // The epic whose wire words the account's row states, for when its table cannot be read (its three
        // `fallback_symbol*` facts; none stated: every epic is read).
        let fallback = crate::execution::FallbackEpic::of(req.inputs.accounts.account_row(
            VENUE,
            req.inputs.account,
            req.inputs.tier,
        ));
        start_with(
            req,
            crate::instrument::mount_contract_value,
            move |config, events, unvalued| {
                Box::new(
                    crate::execution::spawn_sharing(
                        config, events, deal_ids, link, fallback, unvalued,
                    )
                    .with_halt_path(halt_path),
                )
            },
            move |config, symbol| {
                crate::reconcile::reconcile_client_sharing(config, symbol, reconcile_ids)
            },
        )
    }

    /// A labelled account is started only when it armed, so a `live` one never reaches `start`;
    /// `vike-tradehub-core` asks for what `start` would have said (see the trait method).
    fn report_not_running_account(&self, inputs: &AdapterInputs<'_>) {
        Self::arm(inputs).report_live_tier(VENUE, inputs.account);
    }

    /// `GET /session/encryptionKey` with the API key only — no login, no session. The gateway
    /// mirrors the start, which reaches the DEMO gateway and nothing else; the probed account's
    /// key, as `vike-tradehub-core`'s `ig_time` read it. The parse (and its fixture test) lives beside the
    /// request, in `crates/bridges/ig/src/transport.rs`'s `parse_server_time_ms`.
    fn server_time_ms(&self, inputs: &AdapterInputs<'_>, timeout: Duration) -> Result<i64, String> {
        let urls = inputs.settings.urls();
        let Some(config) = load_ig_config_from(AccountTier::Demo, &inputs.credentials, urls)
            .map_err(|refusal| refusal.to_string())?
        else {
            // The FIELDS the account lacks (the loader's own list, so they cannot rot against this
            // crate) — names only, never a value.
            let missing = inputs.credentials.missing(&FIELDS).join(", ");
            return Err(format!(
                "the account's {missing} is not set, so there is no key to read the clock with"
            ));
        };
        crate::transport::IgSession::server_time_ms(&config, timeout).map_err(|e| e.to_string())
    }

    /// The account's working orders, every epic's, read on its RUNNING order session: no login of its own
    /// (`crate::reconcile::venue_read`). The balance and the positions are not offered.
    fn venue_read(
        &self,
        inputs: &AdapterInputs<'_>,
        kind: vike_model::VenueReadKind,
    ) -> Result<vike_model::VenueRead, vike_utility::errors::CarriedRefusal> {
        crate::reconcile::venue_read::venue_read(inputs, kind)
    }
}

#[path = "adapter_tests.rs"]
#[cfg(test)]
mod adapter_tests;
