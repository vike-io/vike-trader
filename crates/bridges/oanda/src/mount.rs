//! oanda's VENUE MOUNT — the `vike_bridge_core::venue_mount::VenueMount` contract over the v20
//! Bearer REST exec client and its dedicated reconcile client
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md).
//!
//! Moved, behaviour unchanged, from `vike-mount`: the `("oanda", _)` arm, its arming-probe row,
//! its clock and book-identity rows.
//!
//! OANDA v20 FX via Bearer REST. LIVE exec when an account below `live` holds its `API_KEY` /
//! `ACCOUNT_ID` fields (`MountInputs::credentials`), else paper. EXEC-ONLY mount, self-gating on
//! [`crate::config::mountable_tier`] — the alpaca/ig mounts' shape plus this venue's own refusal
//! sentence. `OandaExecutionClient::spawn` is INFALLIBLE at mount time (an ExecActor + a
//! self-reconnecting transactions-stream reader — no blocking startup handshake), so there is no
//! connect-failure demotion branch: practice creds ⇒ live, absent ⇒ paper.
//!
//! ⚠ THE REFUSAL, and why this venue had one first when its alpaca/ig siblings did not (they have
//! the same cause and an `error!` of their own now). `oanda_hosts` implements and tests the fxTrade
//! tier, and NOTHING in the workspace ever asks `load_oanda_config_from` for it — so before
//! `mountable_tier` existed, an operator who wrote real fxTrade keys into the store got a venue
//! that stayed PAPER in silence, or REAL orders on the practice account while believing they were
//! live. That is the configured-and-inert shape the credential doctrine names outright: an ABSENT
//! credential is the ordinary unconfigured state and is silent, a PRESENT and unusable one is an
//! ERROR. An account has ONE tier and its credentials are that tier's (decision 0156 item 6), so a
//! `live` account's complete pair is that intent: the mount refuses to select ANY tier for it — the
//! practice hosts included — and says so at `error!`. `resolve` answers it
//! [`PaperCause::LiveTierNotWired`], the cause the final review of the venue programs gave the
//! whole family: this arm answered `NoCredentials` there until then, so `vike-backend venues`
//! printed "no credentials" beside an `error!` saying the keys exist. The `error!` carries `venue`,
//! `account` and `found_tier` (never `tier`, which on a live-mount line is the tier that was
//! mounted), and keeps this venue's own sentence, which names the ACCOUNT and never a credential
//! name or value.
//!
//! WHY REFUSE RATHER THAN WIRE THE TIER. Arming fxTrade here would be a unilateral real-money flip
//! on a bridge whose mount reads no instrument grid (so `RiskLimits` runs on the permissive default —
//! this declaration's `grid_source` is `NoGrid`) and formats every instrument at one fixed
//! precision. The capability-map playbook flips behaviour "one row at a time, behind demo smokes",
//! and no smoke can exist for a tier with no credentials anywhere; ibkr's mount records the same
//! verdict for the same family (its LIVE flip is a deliberate follow-up). A flip also needs the
//! go-live machinery the CEX venues and hyperliquid have (decision 0095: for those venues the
//! network IS the arming ceiling), the `⚠ REAL-MONEY` line, and the budget refusal keyed to it —
//! none of which oanda has grown on its own, and growing it is a shared capability-table
//! extension, by one coordinated PR informed by every consumer, never edited in passing.
//!
//! `error!`, not `warn!`: this is the class of an unreadable store — credentials that EXIST and
//! cannot be used, the class okx's missing-passphrase report also logs at `error!` — and a
//! misconfiguration wearing the "not configured" answer looks exactly like a correct fresh install.
//! The ACCOUNT is logged; the token never is ([`crate::config::UnreachableLiveTier`] holds no
//! value).

use vike_bridge_core::venue_mount::{
    BookIdentity, ClockDecl, DeclaredGridSource, ExecOutcome, HeldBelowLive, LiveExec, MountInputs,
    MountOutcome, MountRequest, PaperCause, Resolution, VenueDeclaration, VenueMount,
    recon_if_enabled,
};
use vike_model::AccountTier;

use crate::config::{MountableTier, UnreachableLiveTier, mountable_tier};
use crate::recon_client::VENUE;

/// oanda's mount. `vike_tradehub::registry::REGISTRY` holds `&OandaVenueMount`.
pub struct OandaVenueMount;

/// **The refusal's one line**, said by `mount` for the account it is mounting and by
/// [`VenueMount::report_unmounted_account`] for a labelled account that is never mounted — so a
/// labelled live account holding its pair is told the same sentence the default account is.
///
/// `found_tier`, not `tier`: `tier` is the tier a mount BOUND (the live-mount convention), and this
/// line says nothing was — the same fields `report_live_tier_not_wired` carries for the arms that
/// share its wording. This sentence stays oanda's own: it names the ACCOUNT (never a credential
/// name or value) and why no fxTrade tier is reachable.
fn report_refusal(
    account: &vike_model::accounts::account_keys::AccountLabel,
    refusal: &UnreachableLiveTier,
) {
    tracing::error!(
        venue = VENUE,
        account = %account,
        found_tier = AccountTier::Live.as_str(),
        "{refusal}"
    );
}

impl OandaVenueMount {
    /// The one decision `resolve`, `mount` and `report_unmounted_account` act on: this account's
    /// pair at this account's tier.
    fn tier_of(inputs: &MountInputs<'_>) -> MountableTier {
        mountable_tier(inputs.account, inputs.tier, &inputs.credentials)
    }
}

impl VenueMount for OandaVenueMount {
    fn venue(&self) -> &'static str {
        VENUE
    }

    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration {
            addresses_accounts: true,
            process_exclusive: None,
            takes_recon_trigger: false,
            // No grid pre-fetch at all today: the mounted symbol keeps `RiskLimits::new()`'s
            // permissive `None`s, so a leg inherits nothing.
            grid_source: DeclaredGridSource::NoGrid,
            // `crate::config`'s `FIELDS`: the `(API_KEY, ACCOUNT_ID)` pair, and the account id IS
            // the account this mount trades.
            book_identity: BookIdentity::Named {
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
            clock: ClockDecl::NotWired {
                reason: "its only time field is the pricing snapshot's publication tick, quantized to \
                         a 1 s grid — a check over it would flap across a ±900 ms band on a healthy \
                         host",
                unmeasured_risk: None,
            },
        }
    }

    /// Only `Practice` arms. A live account's pair answers PAPER on purpose: the mount refuses and
    /// lands on paper, so reporting a session would raise a budget refusal over a venue that can
    /// only be paper (the fxcm probe answers paper where its shim does not load, for the same
    /// reason).
    fn resolve(&self, inputs: &MountInputs<'_>) -> Resolution {
        match Self::tier_of(inputs) {
            MountableTier::Practice(_) => Resolution::Armed {
                tier: AccountTier::Demo,
                held_below_live: Some(HeldBelowLive::DemoOnlyArm),
            },
            // The refusal answers with the cause that NAMES what it refused. It answered
            // `NoCredentials` here until the final review of the venue programs found `venues`
            // printing "no credentials" beside the `error!` below that says the keys exist.
            MountableTier::LiveUnreachable(_) => Resolution::Paper(PaperCause::LiveTierNotWired),
            MountableTier::Unconfigured => Resolution::Paper(PaperCause::NoCredentials),
        }
    }

    fn mount(&self, req: MountRequest<'_>) -> MountOutcome {
        match Self::tier_of(&req.inputs) {
            MountableTier::Practice(cfg) => {
                tracing::warn!(
                    venue = VENUE,
                    account = %req.inputs.account,
                    tier = AccountTier::Demo.as_str(),
                    "oanda: DEMO credentials present → LIVE exec client (real fxPractice orders)"
                );
                // Reconcile handle (recon breadth → OANDA): `crate::recon_client::recon_client`
                // opens its OWN dedicated Bearer `OandaRest` and probes `/summary` (validating the
                // token+account and capturing the fill-floor watermark — a blocking network round
                // trip at mount time, like deribit/ctrader/ig), never the exec transport. Built
                // from `&cfg` HERE, before `cfg` moves into `spawn`. `None` on connect failure
                // stays reconcile-inert; exec unaffected. OANDA reconciles on the periodic
                // INTERVAL only (the declaration's `takes_recon_trigger` is `false`) and is NOT in
                // the daemon's `recon_feed_statuses` → health gate reads Healthy, like
                // deribit/alpaca/ctrader/ig.
                //
                // ORDER-COMPLETENESS (recon safety): `fetch_order_status_reports` reads
                // `/orders?state=PENDING&count=500` — the current resting set — so a local order
                // absent from it is a genuine terminal. The rollout note is IG's: the exposure is
                // `PositionDrift`, not an order-side auto-cancel, so keep this venue at the
                // default `quarantine` policy.
                //
                // LAZY (the `recon_enabled` gate): with reconciliation off the dedicated Bearer
                // `OandaRest` + its blocking `/summary` probe are never built — the factory is
                // not called.
                let recon = recon_if_enabled(req.recon_enabled, || {
                    crate::recon_client::recon_client(&cfg, req.symbol)
                });
                MountOutcome {
                    exec: ExecOutcome::Live(LiveExec {
                        client: Box::new(
                            crate::exec::OandaExecutionClient::spawn(cfg, req.events.clone())
                                .with_halt_path(req.inputs.process.halt_path.clone()),
                        ),
                        bound_tier: AccountTier::Demo,
                        grid: None,
                        contract_size: None,
                        margin_mode: None,
                        leg_grids: Vec::new(),
                    }),
                    recon,
                    identity: None,
                }
            }
            // THE REFUSAL (module doc): paper, nothing to reconcile — the same inert outcome an
            // unconfigured venue reaches, arrived at LOUDLY instead of silently, and reached even
            // though the same pair would have loaded the practice hosts.
            MountableTier::LiveUnreachable(refusal) => {
                report_refusal(req.inputs.account, &refusal);
                MountOutcome::paper()
            }
            // No OANDA creds ⇒ paper, nothing to reconcile. The INERT-DEFAULT path the roster test
            // `all_roster_venues_absent_creds_stay_paper_and_inert` proves for every venue.
            MountableTier::Unconfigured => MountOutcome::paper(),
        }
    }

    /// A labelled account is mounted only when it armed, and a `live` one holding its pair never
    /// does (`resolve` answers it `LiveTierNotWired`) — so `mount` is never reached for it and its
    /// refusal went unsaid. `vike-mount` asks for what `mount` would have said (see the trait
    /// method): the refusal, once, and nothing for any other account.
    fn report_unmounted_account(&self, inputs: &MountInputs<'_>) {
        if let MountableTier::LiveUnreachable(refusal) = Self::tier_of(inputs) {
            report_refusal(inputs.account, &refusal);
        }
    }
}

#[path = "mount_tests.rs"]
#[cfg(test)]
mod mount_tests;
