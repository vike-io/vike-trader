//! **`BridgeBackend` — a test-only `VenueBackend` over [`TradeLaneBridge`], so the tradehub's fold can be
//! driven through the REAL venue half** (wave 2, W9b).
//!
//! Until W9b the roster tests drove `make_engine*` through `LocalBackend`, one crate. The venue half
//! now lives behind the wire in the gatewayhub, and the fold stays in `vike-tradehub-core`, which the
//! gatewayhub's NORMAL closure may not hold (`GATEWAYHUB_FENCE`): this file is the in-process seam the
//! TESTS use, the thin adapter from `TradeLaneBridge`'s verbs to the fold's port, so every moved roster test
//! keeps asserting what it always asserted about the fold + the venue half together. It is the twin of
//! what the tradehub's remote backend does over the socket (W11), minus the socket; the wire twin of
//! these tests is W19's.
//!
//! One file, two includers: the lib's own unit tests (`src/trade/adapters/testutil.rs`, as a child
//! module) and the `trade_roster` group binary (its `main.rs` declares it once, beside the
//! `tests/trade_roster/support/local_backend.rs` built over it).
//! It therefore names the crate as `vike_gatewayhub::…` (the integration binaries link the lib as an
//! external crate); the lib's own unit tests carry a trimmed twin, `src/trade/adapters/testutil.rs`.
#![allow(dead_code, unreachable_pub)]

use std::collections::HashMap;
use std::sync::mpsc::Sender;

use vike_config::ArmingAccount;
use vike_execution::EventSender;
use vike_execution::recon::ReconcileClient;
use vike_gatewayhub::testkit::trade::named_pokes;
use vike_gatewayhub::trade::adapters::bridge_trade::TradeLaneBridge;
use vike_gatewayhub::trade::adapters::registry::VenueAdapterRow;
use vike_gatewayhub::trade::adapters::snapshot::TradeLaneSnapshot;
use vike_gatewayhub::trade::lane::{TradeLane, TradeLaneTarget};
use vike_gatewayhub_client::proto as wire;
use vike_model::ArmingBlock;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{AccountTier, CredentialGap, NetReading, ServerTimeGap};
use vike_tradehub_core::preflight::{
    CANARY_CLOCK_WARN_MS, DEFAULT_CLOCK_FAIL_MS, DEFAULT_CLOCK_WARN_MS, PreflightClockPolicy,
};
use vike_tradehub_core::{
    AdapterBind, AdapterPolicy, AdapterStartQuery, ArmingRow, ArmingTable, PreflightPlan,
    PreflightQuery, VenueAdapterIdentityRecord, VenueBackend, VenueBackendError,
};

/// The venue half's view of the fold's policy: the three fields it reads. Each venue's settings carry
/// its address rows as the daemon's own read does on a box with no `venue_url` table (the release's
/// seed, `vike_secrets::venue_setting::load_venue_settings`) — unless the test states its own
/// addresses, which are kept: replacing them with the seed's would dial the box's real gateway.
#[must_use]
pub fn snapshot_of(policy: &AdapterPolicy) -> TradeLaneSnapshot {
    use vike_venues::venue_urls::VenueUrls;
    let mut venue_settings = vike_secrets::venue_setting::seeded_venue_settings();
    for (venue, settings) in &policy.venue_settings {
        let stated = settings.urls() != &VenueUrls::default();
        let urls = if stated { settings.urls().clone() } else { VenueUrls::seed(venue) };
        venue_settings.insert(venue.clone(), settings.clone().with_urls(urls));
    }
    TradeLaneSnapshot {
        accounts: policy.accounts.clone(),
        venue_settings,
        venue_settings_unread: None,
        preflight_failed: policy.preflight_failed.clone(),
        ..TradeLaneSnapshot::default()
    }
}

/// The in-process backend: a [`TradeLaneBridge`] answering the fold's port.
pub struct BridgeBackend {
    lane: TradeLaneBridge,
}

impl BridgeBackend {
    /// A backend over `registry`, the deployment map, the resolved flags and the policy the test states (its
    /// `preflight_failed` is dropped by the lane, as the fold's overlay owns that stop).
    #[must_use]
    pub fn new(
        registry: &'static [VenueAdapterRow],
        vars: HashMap<String, String>,
        flags: vike_config::Flags,
        policy: &AdapterPolicy,
    ) -> Self {
        let snapshot = TradeLaneSnapshot { flags, ..snapshot_of(policy) };
        BridgeBackend { lane: TradeLaneBridge::with_snapshot(registry, vars, snapshot) }
    }

    /// The lane under it, for a test that asks the lane directly.
    #[must_use]
    pub fn lane(&self) -> &TradeLaneBridge {
        &self.lane
    }
}

/// `wire_word`'s inverse (`ArmingBlock::from_word`): an UNKNOWN word is "adapter failed", never armed.
fn block_of(word: &str) -> ArmingBlock {
    ArmingBlock::from_word(word).unwrap_or(ArmingBlock::AdapterFailed)
}

/// The fold's table from the wire's: every row's venue is a roster id, every label parses. The facts
/// are `vike_model::VenueAdapterDeclarationView` on both sides and cross as they are.
#[must_use]
pub fn table_of(wire: &wire::ArmingTableWire) -> ArmingTable {
    let facts = wire.properties.clone();
    let rows = wire
        .rows
        .iter()
        .map(|r| ArmingRow {
            arming: ArmingAccount {
                venue: vike_model::VENUES
                    .iter()
                    .copied()
                    .find(|v| *v == r.venue)
                    .expect("a table row names a roster venue"),
                label: r.label.as_deref().map_or(AccountLabel::Default, |l| {
                    AccountLabel::parse(l).expect("a legal label")
                }),
                tier: r.tier,
                effective: r.effective,
                block: block_of(&r.block_word),
                account_ids: r.account_ids.clone(),
            },
            book: r.book.as_deref().and_then(|b| vike_model::BookId::new(&r.venue, b)),
        })
        .collect();
    ArmingTable::new(facts, rows)
}

pub fn clock_policy_of(risk: vike_model::ClockRisk) -> PreflightClockPolicy {
    use vike_model::ClockRisk;
    match risk {
        ClockRisk::SignedTimestamp => PreflightClockPolicy {
            warn_ms: DEFAULT_CLOCK_WARN_MS,
            fail_ms: Some(DEFAULT_CLOCK_FAIL_MS),
            remedy: risk.remedy(),
        },
        ClockRisk::NonceWindow | ClockRisk::NoTimestamp => PreflightClockPolicy {
            warn_ms: CANARY_CLOCK_WARN_MS,
            fail_ms: None,
            remedy: risk.remedy(),
        },
    }
}

impl VenueBackend for BridgeBackend {
    fn arming_table(&self) -> Result<ArmingTable, VenueBackendError> {
        // The quiet table: `TradeLane::arming_table` also says what the bridges have to say about the
        // accounts that will not start, once per process, and the fold says that itself through
        // `report_not_running` below, in its own place in the sequence.
        Ok(table_of(&self.lane.arming_table_quiet()))
    }

    fn preflight_plan(&self, q: &PreflightQuery<'_>) -> Result<PreflightPlan, VenueBackendError> {
        let plan = self.lane.preflight_plan(q.clock_venues);
        Ok(PreflightPlan {
            clock_policies: plan
                .clock_risks
                .into_iter()
                .map(|(venue, risk)| (venue, clock_policy_of(risk)))
                .collect(),
            wired_clock_reads: plan.wired_clock_reads,
            credential_venues: plan.credential_venues,
        })
    }

    fn server_time_ms(&self, venue: &str, tier: AccountTier) -> Result<i64, ServerTimeGap> {
        self.lane.server_time_ms(venue, tier)
    }

    fn authed_read(&self, venue: &str) -> Result<(), CredentialGap> {
        self.lane.authed_read(venue)
    }

    fn net_reading(&self) -> NetReading {
        self.lane.net_reading()
    }

    fn report_not_running(&self, venue: &str, rows: &[ArmingAccount]) {
        self.lane.report_not_running(venue, rows);
    }

    fn start_adapter(
        &self,
        q: AdapterStartQuery<'_>,
        events: &EventSender,
        trigger: Option<Sender<String>>,
    ) -> Result<AdapterBind, VenueBackendError> {
        let target = TradeLaneTarget {
            venue: q.venue.to_string(),
            account: q.account.text().map(str::to_string),
            tier: q.tier,
        };
        let start = wire::AdapterStart {
            rid: 0,
            symbol: q.symbol.to_string(),
            declared_legs: q.declared_legs.to_vec(),
            reconcile_enabled: q.reconcile_enabled,
            takes_trigger: trigger.is_some(),
            risk: q.risk_profile.map(|r| wire::AdapterRisk { max_leverage: r.max_leverage }),
            market_slippage: q.market_slippage,
            halt_admit: q.halt_admit,
            withhold: q.withhold,
            account_leverage: q.account_leverage,
            read_account_currency: false,
            route_key: q.route_key.to_string(),
        };
        // The node's poke names the account; the bridge's is bare, so the rig names it as the lease would.
        let trigger = trigger.map(|named| named_pokes(named, q.route_key.to_string()));
        match self.lane.start_adapter(&target, &start, events.clone(), trigger) {
            Ok(started) => Ok(AdapterBind {
                client: started.client,
                reconcile: started.reconcile,
                bound_tier: started.bound_tier,
                grid: started.grid,
                contract_value: started.contract_value,
                margin_mode: started.margin_mode,
                leg_grids: started.leg_grids,
                leverage_cap: None,
                account_currency: None,
                native_time_release: false,
                broker_code_len: 0,
                mount_symbol: None,
            }),
            Err(refusal) => Err(VenueBackendError::NotRunning(block_of(&refusal.block_word))),
        }
    }

    fn record_identity(
        &self,
        q: &VenueAdapterIdentityRecord<'_>,
        recon: Option<&dyn ReconcileClient>,
    ) -> Result<(), VenueBackendError> {
        let target = TradeLaneTarget {
            venue: q.venue.to_string(),
            account: q.account.text().map(str::to_string),
            tier: q.bound_tier,
        };
        self.lane
            .record_identity(&target, q.bound_tier, recon)
            .map_err(VenueBackendError::Unavailable)
    }
}
