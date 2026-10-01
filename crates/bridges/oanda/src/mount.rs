//! oanda's VENUE MOUNT — the `vike_bridge_core::venue_mount::VenueMount` contract over the v20
//! Bearer REST exec client and its dedicated reconcile client
//! (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md).
//!
//! Moved, behaviour unchanged, from `vike-mount`: the `("oanda", _)` arm, its arming-probe row,
//! its clock and book-identity rows.
//!
//! OANDA v20 FX via Bearer REST. LIVE exec when the `OANDA_DEMO_*` config
//! (`OANDA_DEMO_API_KEY`/`_ACCOUNT_ID`) is present, else paper. EXEC-ONLY mount, self-gating on
//! [`crate::config::mountable_tier_for_account`] — the alpaca/ig mounts' shape plus this venue's
//! one refusal. `OandaExecutionClient::spawn` is INFALLIBLE at mount time (an ExecActor + a
//! self-reconnecting transactions-stream reader — no blocking startup handshake), so there is no
//! connect-failure demotion branch: practice creds ⇒ live, absent ⇒ paper.
//!
//! ⚠ THE REFUSAL, and why this venue has one when its alpaca/ig siblings do not. `oanda_hosts`
//! implements and tests the fxTrade tier, and NOTHING in the workspace ever asks
//! `load_oanda_config_from` for it — so before `mountable_tier` existed, an operator who wrote real
//! `OANDA_LIVE_*` keys into the store got a venue that stayed PAPER in silence, and one who wrote
//! BOTH tiers got REAL orders on the practice account while believing they were live. That is the
//! configured-and-inert shape the credential doctrine names outright: an ABSENT credential is the
//! ordinary unconfigured state and is silent, a PRESENT and unusable one is an ERROR. So the mount
//! refuses to select ANY tier from a live-armed store — the practice fallback included — and says
//! so at `error!`.
//!
//! WHY REFUSE RATHER THAN WIRE THE TIER. Arming fxTrade here would be a unilateral real-money flip
//! on a bridge that has no instrument grid at all (so `RiskLimits` runs on the permissive default —
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
//! The NAMES are logged; the token never is ([`crate::config::UnreachableLiveTier`] holds no
//! value).

use vike_bridge_core::venue_mount::{
    BookIdentity, ClockDecl, DeclaredGridSource, ExecOutcome, HeldBelowLive, LiveExec, MountInputs,
    MountOutcome, MountRequest, PaperCause, Resolution, Tier, VenueDeclaration, VenueMount,
    recon_if_enabled,
};

use crate::config::{MountableTier, mountable_tier_for_account};
use crate::recon_client::VENUE;

/// oanda's mount. `vike_tradehub::registry::REGISTRY` holds `&OandaVenueMount`.
pub struct OandaVenueMount;

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
            // `crate::config`'s `tier_var_names`: the `(api_key, account_id)` pair, and the
            // account id IS the fxTrade account this mount trades.
            book_identity: BookIdentity::Named {
                prefix: "OANDA",
                demo_tiers: &["DEMO"],
                live_tiers: &["LIVE", "MAINNET"],
                name_suffixes: &["ACCOUNT_ID"],
                evm_key_suffixes: &[],
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

    /// Only `Practice` arms. A live-armed store answers PAPER on purpose: the mount refuses and
    /// lands on paper, so reporting a session would raise a budget refusal over a venue that can
    /// only be paper (the fxcm probe answers paper where its shim does not load, for the same
    /// reason).
    fn resolve(&self, inputs: &MountInputs<'_>) -> Resolution {
        match mountable_tier_for_account(inputs.account, inputs.secrets) {
            MountableTier::Practice(_) => Resolution::Armed {
                tier: Tier::Demo,
                held_below_live: Some(HeldBelowLive::DemoOnlyArm),
            },
            MountableTier::LiveUnreachable(_) | MountableTier::Unconfigured => {
                Resolution::Paper(PaperCause::NoCredentials)
            }
        }
    }

    fn mount(&self, req: MountRequest<'_>) -> MountOutcome {
        match mountable_tier_for_account(req.inputs.account, req.inputs.secrets) {
            MountableTier::Practice(cfg) => {
                tracing::warn!(
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
                        client: Box::new(crate::exec::OandaExecutionClient::spawn(
                            cfg,
                            req.events.clone(),
                        )),
                        bound_tier: Tier::Demo,
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
            // when `OANDA_DEMO_*` would have loaded.
            MountableTier::LiveUnreachable(refusal) => {
                tracing::error!(venue = VENUE, "{refusal}");
                MountOutcome::paper()
            }
            // No OANDA creds ⇒ paper, nothing to reconcile. The INERT-DEFAULT path the roster test
            // `all_roster_venues_absent_creds_stay_paper_and_inert` proves for every venue.
            MountableTier::Unconfigured => MountOutcome::paper(),
        }
    }
}

#[path = "mount_tests.rs"]
#[cfg(test)]
mod mount_tests;
