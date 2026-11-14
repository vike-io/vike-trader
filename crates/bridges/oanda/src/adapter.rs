//! oanda's VENUE ADAPTER — the `vike_bridge_core::venue_adapter::VenueAdapter` contract over the v20
//! Bearer REST exec client and its dedicated reconcile client
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md).
//!
//! Moved, behaviour unchanged, from `vike-tradehub-core`: the `("oanda", _)` arm, its arming-probe row,
//! its clock and book-identity rows.
//!
//! OANDA v20 FX via Bearer REST. LIVE exec when an account below `live` holds its `API_KEY` /
//! `ACCOUNT_ID` fields (`AdapterInputs::credentials`), else paper. EXEC-ONLY start, self-gating on
//! [`crate::config::demo_only_arm`] (the shared `ArmingDemoOnly`) — the alpaca/ig starts' shape plus this venue's own refusal
//! sentence. `crate::execution`'s `spawn_execution_client_reporting` is INFALLIBLE at start time (an ExecutionClientActor + a
//! self-reconnecting transactions-stream reader — no blocking startup handshake), so there is no
//! connect-failure demotion branch: practice creds ⇒ live, absent ⇒ paper.
//!
//! ⚠ THE REFUSAL, and why this venue had one first when its alpaca/ig siblings did not (they have
//! the same cause and an `error!` of their own now). `oanda_hosts` implements and tests the fxTrade
//! tier, and NOTHING in the workspace ever asks `load_oanda_config_from` for it — so before
//! this refusal existed, an operator who wrote real fxTrade keys into the store got a venue
//! that stayed PAPER in silence, or REAL orders on the practice account while believing they were
//! live. That is the configured-and-inert shape the credential doctrine names outright: an ABSENT
//! credential is the ordinary unconfigured state and is silent, a PRESENT and unusable one is an
//! ERROR. An account has ONE tier and its credentials are that tier's (decision 0156 item 6), so a
//! `live` account's complete pair is that intent: the start refuses to select ANY tier for it — the
//! practice hosts included — and says so at `error!`. `resolve` answers it
//! `ArmingNotRunningCause::LiveTierNotWired`, the cause the final review of the venue programs gave the
//! whole family: this arm answered `NoCredentials` there until then, so `vike-backend venues`
//! printed "no credentials" beside an `error!` saying the keys exist. The `error!` is the shared
//! demo-only arm's (`vike_bridge_core::venue_adapter::report_live_tier_not_wired`, through
//! `ArmingDemoOnly::report_live_tier`, in OANDA's `practice` word; owner ruling B-15 gave this venue the
//! shared phrase): it carries `venue`, `account` and `found_tier` (never `tier`, which on a live-mount
//! line is the tier that was started) and names the ACCOUNT and never a credential name or value.
//!
//! WHY REFUSE RATHER THAN WIRE THE TIER. Arming fxTrade here would be a unilateral real-money flip
//! on a bridge whose start reads no instrument grid for the engine (so `RiskLimits` runs on the
//! permissive default — this declaration's `grid_source` is `NoGrid`; the exec client reads each
//! order's instrument grid itself, `crate::instrument`). The capability-map playbook flips behaviour "one row at a time, behind demo smokes",
//! and no smoke can exist for a tier with no credentials anywhere; ibkr's start records the same
//! verdict for the same family (its LIVE flip is a deliberate follow-up). A flip also needs the
//! go-live machinery the CEX venues and hyperliquid have (decision 0095: for those venues the
//! network IS the arming ceiling), the `⚠ REAL-MONEY` line, and the budget refusal keyed to it —
//! none of which oanda has grown on its own, and growing it is a shared capability-table
//! extension, by one coordinated PR informed by every consumer, never edited in passing.
//!
//! `error!`, not `warn!`: this is the class of an unreadable store — credentials that EXIST and
//! cannot be used, the class okx's missing-passphrase report also logs at `error!` — and a
//! misconfiguration wearing the "not configured" answer looks exactly like a correct fresh install.
//! The ACCOUNT is logged; the token never is (the arm holds no value).

use vike_bridge_core::venue_adapter::{
    AdapterInputs, AdapterOutcome, AdapterRequest, ArmingDemoOnly, ArmingResolution,
    BookIdentityRule, ClockDeclaration, VenueAdapter, VenueAdapterDeclaration,
    VenueAdapterExecutionOutcome, VenueAdapterExecutionRunning, reconcile_if_enabled,
};
use vike_bridge_core::venue_link::{VenueLink, venue_link};
use vike_model::{AccountTier, GridSource};
use vike_venues::venue_urls::UrlRefusal;

use crate::config::{OandaConfig, demo_only_arm};
use crate::reconcile::VENUE;

/// oanda's start. `vike_tradehub::registry::REGISTRY` holds `&VenueAdapterOanda`.
pub struct VenueAdapterOanda;

impl VenueAdapterOanda {
    /// The one decision `resolve`, `start` and `report_not_running_account` act on: this account's
    /// pair at this account's tier.
    fn arm_of(inputs: &AdapterInputs<'_>) -> ArmingDemoOnly<Result<OandaConfig, UrlRefusal>> {
        demo_only_arm(inputs.tier, &inputs.credentials, inputs.settings.urls())
    }
}

impl VenueAdapter for VenueAdapterOanda {
    fn venue(&self) -> &'static str {
        VENUE
    }

    fn declaration(&self) -> VenueAdapterDeclaration {
        VenueAdapterDeclaration {
            addresses_accounts: true,
            process_exclusive: None,
            takes_reconcile_trigger: false,
            // No grid pre-fetch at all today: the engine symbol keeps `RiskLimits::new()`'s
            // permissive `None`s, so a leg inherits nothing.
            grid_source: GridSource::NoGrid,
            // `crate::config`'s `FIELDS`: the `(API_KEY, ACCOUNT_ID)` pair, and the account id IS
            // the account this start trades.
            book_identity: BookIdentityRule::Named {
                name_fields: &["ACCOUNT_ID"],
                evm_key_fields: &[],
            },
            // OANDA publishes a `time` field, and it is NOT a clock. Measured from the CI box
            // 2026-08-08 over four consecutive `GET /v3/accounts/{id}/pricing` calls: the
            // fractions were .300679626, .300892984, .300157847 and .300407597 while the whole
            // seconds went 30 → 31 → 33 → 35 against local seconds 30 → 32 → 33 → 34. It is the
            // pricing publication tick on a 1 s grid at a fixed .300 phase, so the derived "skew"
            // swung from -884 ms to +17 ms on a disciplined host. The other two endpoints this
            // adapter calls (`/v3/accounts`, `/v3/accounts/{id}/summary`) carry no time field at
            // all, and OANDA's Bearer auth stamps no timestamp on a request.
            clock: ClockDeclaration::NotWired {
                reason: "its only time field is the pricing snapshot's publication tick, quantized to \
                         a 1 s grid — a check over it would flap across a ±900 ms band on a healthy \
                         host",
                unmeasured_risk: None,
            },
        }
    }

    /// Only `Practice` arms. A live account's pair answers PAPER on purpose: the start refuses and
    /// lands on paper, so reporting a session would raise a budget refusal over a venue that can
    /// only be paper (the fxcm probe answers paper where its shim does not load, for the same
    /// reason).
    fn resolve(&self, inputs: &AdapterInputs<'_>) -> ArmingResolution {
        // `Armed { Demo, ArmingDemoOnly }` for the practice tier; the refusal answers with the cause that NAMES what
        // it refused (`LiveTierNotWired`). It answered `NoCredentials` there until the final review of the venue
        // programs found `venues` printing "no credentials" beside the `error!` `start` says, that the keys exist.
        Self::arm_of(inputs).resolution()
    }

    fn start(&self, req: AdapterRequest<'_>) -> AdapterOutcome {
        // The demo-only start's prefix (`ArmingDemoOnly::config_for_start`): THE REFUSAL (module doc) of a `live`
        // account's pair, in the shared phrase, and a practice pair whose address rows were refused, in the
        // refusal's words (they name the row), each an `error!`, then paper with nothing to reconcile, the
        // same inert outcome an unconfigured venue reaches silently (the INERT-DEFAULT path the roster test
        // `all_roster_venues_absent_credentials_stay_paper_and_inert` proves for every venue).
        let Some(config) = Self::arm_of(&req.inputs).config_for_start(VENUE, req.inputs.account)
        else {
            return AdapterOutcome::paper();
        };
        tracing::warn!(
            venue = VENUE,
            account = %req.inputs.account,
            tier = AccountTier::Demo.as_str(),
            "oanda: DEMO credentials present → LIVE exec client (real fxPractice orders)"
        );
        // Reconcile handle (recon breadth → OANDA): `crate::reconcile::recon_client`
        // opens its OWN dedicated Bearer `OandaTransport` and probes `/summary` (validating the
        // token+account and capturing the fill-floor watermark, or reading the floor the last
        // run left in the declared state directory (`crate::reconcile::fill_floor`) — a blocking network
        // round trip at start time, like deribit/ctrader/ig), never the exec transport. Built
        // from `&config` HERE, before `config` moves into `spawn`. `None` on connect failure
        // stays reconcile-inert; exec unaffected. OANDA reconciles on the periodic
        // INTERVAL only (the declaration's `takes_reconcile_trigger` is `false`) and is NOT in
        // the daemon's `reconcile_feed_statuses` → health gate reads Healthy, like
        // deribit/alpaca/ctrader/ig.
        //
        // ORDER-COMPLETENESS (recon safety): `fetch_order_status_reports` reads
        // `/orders?state=PENDING&count=500` — the current resting set — so a local order
        // absent from it is a genuine terminal. The rollout note is IG's: the exposure is
        // `PositionDrift`, not an order-side auto-cancel, so keep this venue at the
        // default `quarantine` policy.
        //
        // LAZY (the `recon_enabled` gate): with reconciliation off the dedicated Bearer
        // `OandaTransport` + its blocking `/summary` probe are never built — the factory is
        // not called.
        let recon = reconcile_if_enabled(req.reconcile_enabled, || {
            crate::reconcile::recon_client(
                &config,
                req.symbol,
                req.inputs.process.state_dir.as_deref(),
            )
        });
        AdapterOutcome {
            exec: VenueAdapterExecutionOutcome::Running(VenueAdapterExecutionRunning {
                client: Box::new(
                    crate::execution::spawn_execution_client_reporting(
                        config,
                        req.events.clone(),
                        // The account's ONE link cell, by the route key its lane carries (a bare lane,
                        // which names no account, states itself on a cell nobody reads).
                        req.events
                            .route_key
                            .map_or_else(VenueLink::default, |key| venue_link(&key)),
                    )
                    .with_halt_path(req.inputs.process.halt_path.clone()),
                ),
                bound_tier: AccountTier::Demo,
                grid: None,
                contract_value: None,
                margin_mode: None,
                leg_grids: Vec::new(),
            }),
            reconcile: recon,
            identity: None,
        }
    }

    /// A labelled account is started only when it armed, and a `live` one holding its pair never
    /// does (`resolve` answers it `LiveTierNotWired`) — so `start` is never reached for it and its
    /// refusal went unsaid. `vike-tradehub-core` asks for what `start` would have said (see the trait
    /// method): the refusal, once, and nothing for any other account.
    fn report_not_running_account(&self, inputs: &AdapterInputs<'_>) {
        Self::arm_of(inputs).report_live_tier(VENUE, inputs.account);
    }

    /// The account's pending orders, every instrument's (`crate::reconcile::venue_read`), on the practice host this arm
    /// would start on; a `live` account is told so with nothing sent. The balance and the positions are not
    /// offered.
    fn venue_read(
        &self,
        inputs: &AdapterInputs<'_>,
        kind: vike_model::VenueReadKind,
    ) -> Result<vike_model::VenueRead, String> {
        if kind != vike_model::VenueReadKind::OpenOrders {
            return Err(vike_model::not_offered_words(VENUE, kind));
        }
        match Self::arm_of(inputs) {
            ArmingDemoOnly::Demo(Ok(config)) => crate::reconcile::venue_read::open_orders(
                &config,
                &crate::reconcile::AUTH_RETRY_PAUSES,
            ),
            ArmingDemoOnly::Demo(Err(refusal)) => Err(refusal.to_string()),
            ArmingDemoOnly::LiveTierNotWired => {
                Err("this build reaches no fxTrade host, so an account on the live tier has no \
                     host to read; nothing was asked"
                    .to_string())
            }
            ArmingDemoOnly::Unconfigured => {
                Err("the account holds no oanda API_KEY and ACCOUNT_ID pair; nothing was asked"
                    .to_string())
            }
        }
    }
}

#[path = "adapter_tests.rs"]
#[cfg(test)]
mod adapter_tests;
