//! **`BridgeBackend` — a test-only `VenueBackend` over [`BridgeTrade`], so the tradehub's fold can be
//! driven through the REAL venue half** (wave 2, W9b).
//!
//! Until W9b the roster tests drove `make_engine*` through `LocalBackend`, one crate. The venue half
//! now lives behind the wire in the gatewayhub, and the fold stays in `vike-tradehub-core`, which the
//! gatewayhub's NORMAL closure may not hold (`GATEWAYHUB_FENCE`): this file is the in-process seam the
//! TESTS use, the thin adapter from `BridgeTrade`'s verbs to the fold's port, so every moved roster test
//! keeps asserting what it always asserted about the fold + the venue half together. It is the twin of
//! what the tradehub's remote backend does over the socket (W11), minus the socket; the wire twin of
//! these tests is W19's.
//!
//! One file, two includers: the lib's own unit tests (`src/trade/adapters/testutil.rs`, as a child
//! module) and the integration binaries (`tests/trade_roster/support/local_backend.rs`, by `#[path]`).
//! It therefore names the crate as `vike_gatewayhub::…` (the integration binaries link the lib as an
//! external crate); the lib's own unit tests carry a trimmed twin, `src/trade/adapters/testutil.rs`.
#![allow(dead_code, unreachable_pub)]

use std::collections::HashMap;
use std::sync::mpsc::Sender;

use vike_config::{ArmingBlock, VenueArming};
use vike_exec::EventSender;
use vike_exec::recon::ReconClient;
use vike_gatewayhub::trade::adapters::arming::wire_word;
use vike_gatewayhub::trade::adapters::bridge_trade::BridgeTrade;
use vike_gatewayhub::trade::adapters::probe::{CredentialGap, NetReading, ServerTimeGap};
use vike_gatewayhub::trade::adapters::registry::VenueRow;
use vike_gatewayhub::trade::adapters::snapshot::TradeSnapshot;
use vike_gatewayhub::trade::lane::{TradeLane, TradeTarget};
use vike_gatewayhub_client::proto as wire;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub_core::preflight::{
    CANARY_CLOCK_WARN_MS, ClockPolicy, DEFAULT_CLOCK_FAIL_MS, DEFAULT_CLOCK_WARN_MS,
};
use vike_tradehub_core::{
    AdapterBind, AdapterPolicy, AdapterStartQuery, ArmingRow, ArmingTable, BackendError,
    IdentityRecord, PreflightPlan, PreflightQuery, VenueBackend, VenueFacts, Withhold,
};

/// The venue half's view of the fold's policy: the three fields it reads.
#[must_use]
pub fn snapshot_of(policy: &AdapterPolicy) -> TradeSnapshot {
    TradeSnapshot {
        accounts: policy.accounts.clone(),
        venue_settings: policy.venue_settings.clone(),
        venue_settings_unread: None,
        preflight_failed: policy.preflight_failed.clone(),
    }
}

/// The in-process backend: a [`BridgeTrade`] answering the fold's port.
pub struct BridgeBackend {
    lane: BridgeTrade,
}

impl BridgeBackend {
    /// A backend over `registry`, the deployment map and the policy the test states (its
    /// `preflight_failed` is dropped by the lane, as the fold's overlay owns that stop).
    #[must_use]
    pub fn new(
        registry: &'static [VenueRow],
        vars: HashMap<String, String>,
        policy: &AdapterPolicy,
    ) -> Self {
        BridgeBackend { lane: BridgeTrade::with_snapshot(registry, vars, snapshot_of(policy)) }
    }

    /// The lane under it, for a test that asks the lane directly.
    #[must_use]
    pub fn lane(&self) -> &BridgeTrade {
        &self.lane
    }
}

/// `wire_word`'s inverse for the tests (W11's `from_word` is the production one): an
/// UNKNOWN word is "adapter failed", never armed.
fn block_of(word: &str) -> ArmingBlock {
    ArmingBlock::ALL
        .iter()
        .copied()
        .find(|b| wire_word(*b) == word)
        .unwrap_or(ArmingBlock::AdapterFailed)
}

fn grid_of(g: wire::GridSource) -> vike_tradehub_core::symbol_grid::GridSource {
    use vike_tradehub_core::symbol_grid::GridSource;
    match g {
        wire::GridSource::InHand => GridSource::InHand,
        wire::GridSource::PerSymbolFetch => GridSource::PerSymbolFetch,
        wire::GridSource::NoGrid => GridSource::NoGrid,
    }
}

fn facts_of(f: &wire::VenueFacts) -> VenueFacts {
    VenueFacts {
        adapter_present: f.adapter_present,
        addresses_accounts: f.addresses_accounts,
        takes_recon_trigger: f.takes_recon_trigger,
        grid_source: grid_of(f.grid_source),
        process_exclusive: f.process_exclusive.clone(),
        clock_decl_word: match f.clock_decl_word.as_str() {
            "wired" => "wired",
            "not-wired" => "not-wired",
            _ => "absent",
        },
    }
}

/// The fold's table from the wire's: every row's venue is a roster id, every label parses.
#[must_use]
pub fn table_of(wire: &wire::ArmingTable) -> ArmingTable {
    let facts = wire.facts.iter().map(|(v, f)| (v.clone(), facts_of(f))).collect();
    let rows = wire
        .rows
        .iter()
        .map(|r| ArmingRow {
            arming: VenueArming {
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
            book: r.book.clone(),
        })
        .collect();
    ArmingTable::new(facts, rows)
}

pub fn clock_policy_of(risk: vike_bridge_core::venue_adapter::ClockRisk) -> ClockPolicy {
    use vike_bridge_core::venue_adapter::ClockRisk;
    match risk {
        ClockRisk::SignedTimestamp => ClockPolicy {
            warn_ms: DEFAULT_CLOCK_WARN_MS,
            fail_ms: Some(DEFAULT_CLOCK_FAIL_MS),
            remedy: risk.remedy(),
        },
        ClockRisk::NonceWindow | ClockRisk::NoTimestamp => {
            ClockPolicy { warn_ms: CANARY_CLOCK_WARN_MS, fail_ms: None, remedy: risk.remedy() }
        }
    }
}

fn gap_of(gap: ServerTimeGap) -> vike_tradehub_core::preflight::ServerTimeGap {
    use vike_tradehub_core::preflight::ServerTimeGap as Core;
    match gap {
        ServerTimeGap::Unreachable(why) => Core::Unreachable(why),
        ServerTimeGap::NotChecked(reason) => Core::NotChecked(reason),
        ServerTimeGap::UnmeasuredRisk { reason, at_stake } => {
            Core::UnmeasuredRisk { reason, at_stake }
        }
    }
}

fn credential_gap_of(gap: CredentialGap) -> vike_tradehub_core::preflight::CredentialGap {
    use vike_tradehub_core::preflight::CredentialGap as Core;
    match gap {
        CredentialGap::Rejected(why) => Core::Rejected(why),
        CredentialGap::Unanswered { waited_ms, detail } => Core::Unanswered { waited_ms, detail },
    }
}

impl VenueBackend for BridgeBackend {
    fn arming_table(&self) -> Result<ArmingTable, BackendError> {
        // The quiet table: `TradeLane::arming_table` also says what the bridges have to say about the
        // accounts that will not start, once per process, and the fold says that itself through
        // `report_not_running` below, in its own place in the sequence.
        Ok(table_of(&self.lane.arming_table_quiet()))
    }

    fn preflight_plan(&self, q: &PreflightQuery<'_>) -> Result<PreflightPlan, BackendError> {
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

    fn server_time_ms(
        &self,
        venue: &str,
        tier: AccountTier,
    ) -> Result<i64, vike_tradehub_core::preflight::ServerTimeGap> {
        self.lane.server_time_ms(venue, tier).map_err(gap_of)
    }

    fn authed_read(&self, venue: &str) -> Result<(), vike_tradehub_core::preflight::CredentialGap> {
        self.lane.authed_read(venue).map_err(credential_gap_of)
    }

    fn net_reading(&self) -> vike_tradehub_core::NetReading {
        use vike_tradehub_core::NetReading as Core;
        match self.lane.net_reading() {
            NetReading::Unknown => Core::Unknown,
            NetReading::Alive { rounds } => Core::Alive { rounds },
            NetReading::Dead { since_ms } => Core::Dead { since_ms },
        }
    }

    fn report_not_running(&self, venue: &str, rows: &[VenueArming]) {
        self.lane.report_not_running(venue, rows);
    }

    fn start_adapter(
        &self,
        q: AdapterStartQuery<'_>,
        events: &EventSender,
        trigger: Option<Sender<()>>,
    ) -> Result<AdapterBind, BackendError> {
        let target = TradeTarget {
            venue: q.venue.to_string(),
            account: q.account.text().map(str::to_string),
            tier: q.tier,
        };
        let start = wire::AdapterStart {
            rid: 0,
            symbol: q.symbol.to_string(),
            declared_legs: q.declared_legs.to_vec(),
            recon_enabled: q.recon_enabled,
            takes_trigger: trigger.is_some(),
            risk: q.risk_profile.map(|r| wire::AdapterRisk { max_leverage: r.max_leverage }),
            market_slippage: q.market_slippage,
            halt_admit: q.halt_admit,
            withhold: q.withhold.map(|w| match w {
                Withhold::PreflightFailed => wire::Withhold::PreflightFailed,
                Withhold::DataOnly => wire::Withhold::DataOnly,
            }),
            account_leverage: q.account_leverage,
            read_account_ccy: false,
            route_key: q.route_key.to_string(),
        };
        match self.lane.start_adapter(&target, &start, events.clone(), trigger) {
            Ok(started) => Ok(AdapterBind {
                client: started.client,
                recon: started.recon,
                bound_tier: started.bound_tier,
                grid: started.grid,
                contract_value: started.contract_value,
                margin_mode: started.margin_mode,
                leg_grids: started.leg_grids,
                confirmed_book: None,
                leverage_cap: None,
                account_ccy: None,
                account_ccy_assumed: false,
                mount_symbol: None,
            }),
            Err(refusal) => Err(BackendError::NotRunning(block_of(&refusal.block_word))),
        }
    }

    fn record_identity(
        &self,
        q: &IdentityRecord<'_>,
        recon: Option<&dyn ReconClient>,
    ) -> Result<(), BackendError> {
        let target = TradeTarget {
            venue: q.venue.to_string(),
            account: q.account.text().map(str::to_string),
            tier: q.bound_tier,
        };
        self.lane.record_identity(&target, q.bound_tier, recon).map_err(BackendError::Unavailable)
    }
}
