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

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender;

use vike_config::{ArmingBlock, VenueArming};
use vike_exec::EventSender;
use vike_exec::recon::ReconClient;
use vike_gatewayhub_client::proto as wire;
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub_core::{
    AdapterBind, AdapterEnv, AdapterPolicy, AdapterStartQuery, ArmingRow, ArmingTable,
    BackendError, IdentityRecord, PreflightPlan, PreflightQuery, VenueBackend, VenueFacts,
    Withhold,
};

use super::arming::wire_word;
use super::bridge_trade::BridgeTrade;
use super::registry::VenueRow;
use super::snapshot::TradeSnapshot;
use crate::trade::lane::{TradeLane, TradeTarget};

/// The venue half's view of the fold's policy: the three fields it reads.
pub(crate) fn snapshot_of(policy: &AdapterPolicy) -> TradeSnapshot {
    TradeSnapshot {
        accounts: policy.accounts.clone(),
        venue_settings: policy.venue_settings.clone(),
        venue_settings_unread: None,
        preflight_failed: policy.preflight_failed.clone(),
    }
}

/// `wire_word`'s inverse: an UNKNOWN word is "adapter failed", never armed.
fn block_of(word: &str) -> ArmingBlock {
    ArmingBlock::ALL
        .iter()
        .copied()
        .find(|b| wire_word(*b) == word)
        .unwrap_or(ArmingBlock::AdapterFailed)
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

/// See the module doc.
pub(crate) struct Rig {
    backend: BridgeBackend,
    table: ArmingTable,
}

impl Rig {
    /// A rig over `registry`, the deployment map and the policy a test states (`None` reads the
    /// default policy: no account table, so no account runs).
    pub(crate) fn new(
        registry: &'static [VenueRow],
        vars: &HashMap<String, String>,
        policy: Option<&AdapterPolicy>,
    ) -> Self {
        let backend =
            BridgeBackend::new(registry, vars.clone(), &policy.cloned().unwrap_or_default());
        let table = backend.arming_table().expect("the local backend always answers");
        Rig { backend, table }
    }

    /// The env a start runs in: this rig's backend and table, nothing else set.
    pub(crate) fn env<'a>(
        &'a self,
        live_events: &'a vike_exec::EventSender,
        live_venues: &'a mut HashSet<String>,
    ) -> AdapterEnv<'a> {
        AdapterEnv::new(&self.backend, &self.table, live_events, live_venues)
    }

    /// The arming snapshot the rig's backend answered.
    pub(crate) fn table(&self) -> &ArmingTable {
        &self.table
    }
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
    let rig = Rig::new(registry, vars, policy);
    vike_tradehub_core::venue_account_arming(rig.table(), venue, policy)
}

/// An `ExecutionClient` whose `submit` and `cancel` do nothing: a live outcome's client where the
/// test judges the fold, never an order.
pub(crate) struct NoopClient;
impl vike_exec::ExecutionClient for NoopClient {
    fn submit(&mut self, _request: &vike_model::OrderRequest) {}
    fn cancel(&mut self, _client_order_id: &str) {}
}

/// An `ExecutionClient` whose `submit` panics: a bridge bug, for the paths that end an adapter by one.
pub(crate) struct PanickingClient;
impl vike_exec::ExecutionClient for PanickingClient {
    fn submit(&mut self, _request: &vike_model::OrderRequest) {
        panic!("a bridge bug while submitting");
    }
    fn cancel(&mut self, _client_order_id: &str) {}
}

/// The three report fetches every `ReconClient` must spell, each answering `Ok(vec![])`. A test
/// client that answers one OPTIONAL fetch itself (`fetch_balance`, `fetch_fee_rates`) invokes this
/// inside its `impl` block and spells only that method.
macro_rules! empty_report_fetches {
    () => {
        fn fetch_order_status_reports(
            &self,
            _since: i64,
        ) -> Result<Vec<vike_model::OrderStatusReport>, String> {
            Ok(vec![])
        }
        fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<vike_model::FillReport>, String> {
            Ok(vec![])
        }
        fn fetch_position_status_reports(
            &self,
        ) -> Result<Vec<vike_model::PositionStatusReport>, String> {
            Ok(vec![])
        }
    };
}
pub(crate) use empty_report_fetches;

/// A `ReconClient` with no reports; every optional fetch keeps the trait's `Ok(None)` default.
pub(crate) struct EmptyRecon;
impl vike_exec::recon::ReconClient for EmptyRecon {
    empty_report_fetches!();
}
