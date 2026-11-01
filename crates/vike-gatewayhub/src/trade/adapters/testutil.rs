//! **The test rig of the venue half's fold tests**: a [`BridgeBackend`] over a planted registry and the
//! fold's arming table, built from the SAME inputs, so a test drives the real fold (`make_engine*`, which
//! lives in `vike-tradehub-core`, a DEV dependency of this crate) through a planted venue exactly as the
//! roster tests (`tests/trade_roster/`) drive it through the real registry. The env it hands out has no
//! policy: a test that states one sets `env.policy`, as it always did, and passes the same policy here.
//!
//! [`BridgeBackend`] is the in-crate half of the pair `tests/trade_roster/support/bridge_backend.rs`
//! completes: the same adapter from `BridgeTrade`'s verbs to the fold's port, here with the four verbs
//! the fold-driving unit tests reach (table, start, identity, not-running report). The preflight verbs
//! are the roster tests' (`mount_roster/startup.rs`, `startup_preflight_smoke.rs`), which use the full
//! file. They are two copies because the lib's own tests name the crate as `crate::` and the
//! integration binaries as `vike_gatewayhub::`: two crate instances, two sets of types.

use std::collections::HashMap;
use std::sync::mpsc::Sender;

use vike_config::{ArmingBlock, VenueArming};
use vike_exec::EventSender;
use vike_exec::recon::ReconClient;
use vike_gatewayhub_client::proto as wire;
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub_core::test_support::rig::Rig;
use vike_tradehub_core::{
    AdapterBind, AdapterPolicy, AdapterStartQuery, ArmingRow, ArmingTable, BackendError,
    IdentityRecord, PreflightPlan, PreflightQuery, VenueBackend, VenueFacts, Withhold,
};

use super::bridge_trade::BridgeTrade;
use super::registry::VenueRow;
use super::snapshot::TradeSnapshot;
use crate::trade::lane::{TradeLane, TradeTarget};

/// The start the adapter unit suites send an `ig` lane: EURUSD, no recon, request id `rid` (the rest is
/// the trade rig's [`crate::testkit::trade_rig::start_args`]).
pub(crate) fn ig_start(rid: u64) -> wire::AdapterStart {
    wire::AdapterStart {
        rid,
        symbol: "EURUSD".into(),
        recon_enabled: false,
        route_key: "ig".into(),
        ..crate::testkit::trade_rig::start_args()
    }
}

/// The venue half's view of the fold's policy: the three fields it reads.
pub(crate) fn snapshot_of(policy: &AdapterPolicy) -> TradeSnapshot {
    TradeSnapshot {
        accounts: policy.accounts.clone(),
        venue_settings: policy.venue_settings.clone(),
        venue_settings_unread: None,
        preflight_failed: policy.preflight_failed.clone(),
    }
}

/// `wire_word`'s inverse (`ArmingBlock::from_word`): an UNKNOWN word is "adapter failed", never armed.
fn block_of(word: &str) -> ArmingBlock {
    ArmingBlock::from_word(word).unwrap_or(ArmingBlock::AdapterFailed)
}

fn facts_of(f: &wire::VenueFacts) -> VenueFacts {
    use vike_tradehub_core::symbol_grid::GridSource;
    VenueFacts {
        adapter_present: f.adapter_present,
        addresses_accounts: f.addresses_accounts,
        takes_recon_trigger: f.takes_recon_trigger,
        grid_source: match f.grid_source {
            wire::GridSource::InHand => GridSource::InHand,
            wire::GridSource::PerSymbolFetch => GridSource::PerSymbolFetch,
            wire::GridSource::NoGrid => GridSource::NoGrid,
        },
        process_exclusive: f.process_exclusive.clone(),
        clock_decl_word: match f.clock_decl_word.as_str() {
            "wired" => "wired",
            "not-wired" => "not-wired",
            _ => "absent",
        },
    }
}

/// The fold's table from the wire's.
fn table_of(table: &wire::ArmingTable) -> ArmingTable {
    let facts = table.facts.iter().map(|(v, f)| (v.clone(), facts_of(f))).collect();
    let rows = table
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

/// A [`BridgeTrade`] answering the fold's port (see the module doc for what it leaves out).
pub(crate) struct BridgeBackend {
    lane: BridgeTrade,
}

impl BridgeBackend {
    pub(crate) fn new(
        registry: &'static [VenueRow],
        vars: HashMap<String, String>,
        policy: &AdapterPolicy,
    ) -> Self {
        BridgeBackend { lane: BridgeTrade::with_snapshot(registry, vars, snapshot_of(policy)) }
    }
}

impl VenueBackend for BridgeBackend {
    fn arming_table(&self) -> Result<ArmingTable, BackendError> {
        Ok(table_of(&self.lane.arming_table_quiet()))
    }

    fn preflight_plan(&self, _q: &PreflightQuery<'_>) -> Result<PreflightPlan, BackendError> {
        unreachable!("the fold-driving unit tests never run the preflight")
    }

    fn server_time_ms(
        &self,
        _venue: &str,
        _tier: vike_model::AccountTier,
    ) -> Result<i64, vike_tradehub_core::preflight::ServerTimeGap> {
        unreachable!("the fold-driving unit tests never run the preflight")
    }

    fn authed_read(
        &self,
        _venue: &str,
    ) -> Result<(), vike_tradehub_core::preflight::CredentialGap> {
        unreachable!("the fold-driving unit tests never run the preflight")
    }

    fn net_reading(&self) -> vike_tradehub_core::NetReading {
        unreachable!("the fold-driving unit tests never run the preflight")
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

/// A rig (`vike_tradehub_core::test_support::rig`, see the module doc) over `registry`, the deployment
/// map and the policy a test states (`None` reads the default policy: no account table, so no account runs).
pub(crate) fn rig(
    registry: &'static [VenueRow],
    vars: &HashMap<String, String>,
    policy: Option<&AdapterPolicy>,
) -> Rig<BridgeBackend> {
    Rig::over(BridgeBackend::new(registry, vars.clone(), &policy.cloned().unwrap_or_default()))
}

/// The per-ACCOUNT rows of `venue` as the fold reads them: a table built from `registry`, the
/// deployment map and the policy, with the startup preflight's stop laid over it. The argument list
/// of the registry-taking `venue_account_arming` this replaced, so a test reads as it always did.
pub(crate) fn rows_of(
    registry: &'static [VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
    policy: Option<&AdapterPolicy>,
) -> Vec<VenueArming> {
    rig(registry, vars, policy).rows(venue, policy)
}

/// A planted declaration that addresses accounts and claims `resource` process-exclusively (the
/// exclusive-claim folds' planted bridges).
pub(crate) fn exclusive_declaration(
    resource: &'static str,
) -> vike_bridge_core::venue_adapter::VenueDeclaration {
    vike_bridge_core::venue_adapter::VenueDeclaration {
        addresses_accounts: true,
        process_exclusive: Some(vike_bridge_core::venue_adapter_fixture::planted_exclusive(
            resource,
        )),
        ..vike_bridge_core::venue_adapter_fixture::PLANTED_DECLARATION
    }
}

/// A plain IBKR stock's facts (cent ticks, whole shares, no magnifier) under `name`: the per-name lookup
/// suites' answer (`ibkr_facts_tests`, `ibkr_rate_tests`; neither reads the conid).
#[cfg(feature = "ibkr")]
pub(crate) fn ibkr_stock(name: &str) -> vike_ibkr::any_symbol::StockFacts {
    vike_ibkr::any_symbol::StockFacts {
        symbol: name.into(),
        conid: 265_598,
        properties: vike_model::SymbolProperties {
            tick_size: 0.01,
            step_size: 1.0,
            min_qty: 1.0,
            ..vike_model::SymbolProperties::default()
        },
        price_magnifier: 1,
        tick_grid: vike_ibkr::any_symbol::TickGrid::default(),
    }
}

/// An `ExecutionClient` whose `submit` panics: a bridge bug, for the paths that end an adapter by one.
/// (The do-nothing client and the empty reconcile client are `vike_tradehub_core::test_support::doubles`.)
pub(crate) struct PanickingClient;
impl vike_exec::ExecutionClient for PanickingClient {
    fn submit(&mut self, _request: &vike_model::OrderRequest) {
        panic!("a bridge bug while submitting");
    }
    fn cancel(&mut self, _client_order_id: &str) {}
}
