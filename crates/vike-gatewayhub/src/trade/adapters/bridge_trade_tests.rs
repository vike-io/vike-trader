//! `TradeLaneBridge`'s own behaviour: what it adds to the moved venue half (the wire-shaped answers), over PLANTED
//! rows. The venue half's rules are `contract_tests/`, `exclusive_*tests` and `startup_tests/`; the real
//! bridges' arming is `tests/trade_roster/`.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};

use vike_bridge_core::venue_adapter::{
    AdapterInputs, AdapterOutcome, AdapterRequest, ArmingNotRunningCause, ArmingResolution,
    ClockAuthentication, ClockDeclaration, VenueAdapter, VenueAdapterDeclaration,
    VenueAdapterExecutionOutcome, VenueAdapterExecutionRunning,
};
use vike_bridge_core::venue_adapter_fixture::{PLANTED_DECLARATION, PlantedAdapter};
use vike_gatewayhub_client::proto::{
    AdapterStart, CredentialGapWire, NetReadingWire, PROPERTIES_LIST_UNAVAILABLE,
    PROPERTIES_RATE_UNAVAILABLE, PreflightLegWire, PreflightStep, ServerTimeGapWire,
    SymbolPropertiesRefusalCode, SymbolPropertiesWire, VenueLinkWire,
};
use vike_model::ArmingBlock;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{AccountTier, ClockRisk};

use super::bridge_trade::TradeLaneBridge;
use super::probe::{clock_result, credential_result, network_result};
use super::registry::VenueAdapterRow;
use crate::trade::lane::{TradeLane, TradeLaneTarget, account_currency_wire};
use vike_model::{CredentialGap, NetReading};

/// A venue that is present, resolves paper for want of credentials and counts how often its bridge was asked
/// to speak for an account that will not start.
struct Speaks {
    spoke: AtomicUsize,
}

impl VenueAdapter for Speaks {
    fn venue(&self) -> &'static str {
        "bybit"
    }
    fn declaration(&self) -> VenueAdapterDeclaration {
        PLANTED_DECLARATION
    }
    fn resolve(&self, _inputs: &AdapterInputs<'_>) -> ArmingResolution {
        ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials)
    }
    fn start(&self, _req: AdapterRequest<'_>) -> AdapterOutcome {
        AdapterOutcome::paper()
    }
    fn report_not_running_account(&self, _inputs: &AdapterInputs<'_>) {
        self.spoke.fetch_add(1, Ordering::SeqCst);
    }
}

static SPEAKS: Speaks = Speaks { spoke: AtomicUsize::new(0) };
static SPEAKS_REG: [VenueAdapterRow; 1] = [VenueAdapterRow::Adapter(&SPEAKS)];

static PLAIN: PlantedAdapter = PlantedAdapter::new(
    "binance",
    ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials),
);
static MIXED_REG: [VenueAdapterRow; 2] = [
    VenueAdapterRow::Adapter(&PLAIN),
    VenueAdapterRow::FeatureAbsent { venue: "ibkr", feature: "ibkr" },
];

fn lane(
    registry: &'static [VenueAdapterRow],
    policy: &vike_tradehub_core::AdapterPolicy,
) -> TradeLaneBridge {
    TradeLaneBridge::with_snapshot(registry, HashMap::new(), super::testutil::snapshot_of(policy))
}

fn start() -> AdapterStart {
    AdapterStart {
        rid: 1,
        symbol: "BTCUSDT".to_string(),
        declared_legs: Vec::new(),
        reconcile_enabled: false,
        takes_trigger: false,
        risk: None,
        market_slippage: None,
        halt_admit: vike_model::HaltAdmit::Admit,
        withhold: None,
        account_leverage: None,
        read_account_currency: false,
        route_key: "binance".to_string(),
    }
}

/// A start from a node that ARMED the conversion for the account (`read_account_currency`: demo, an exposure ceiling).
fn start_reading_currency() -> AdapterStart {
    AdapterStart { read_account_currency: true, ..start() }
}

fn target(venue: &str, tier: AccountTier) -> TradeLaneTarget {
    TradeLaneTarget { venue: venue.to_string(), account: None, tier }
}

fn events() -> vike_execution::EventSender {
    vike_execution::event_channel(8).0
}

/// `venues()` is a BUILD fact: the adapters this build carries, never a `FeatureAbsent` row.
#[test]
fn venues_are_the_adapter_rows_only() {
    let lane = lane(&MIXED_REG, &vike_tradehub_core::AdapterPolicy::default());
    assert_eq!(lane.venues(), ["binance"]);
}

/// A label the wire spelling does not admit is an account with no row, and the default account of a table
/// with no rows is the same answer: the lane never guesses a tier.
#[test]
fn an_unparsable_label_and_a_missing_row_are_no_account_row() {
    let lane = lane(&MIXED_REG, &vike_tradehub_core::AdapterPolicy::default());
    assert_eq!(lane.account_tier("binance", Some("not a label")), Err(ArmingBlock::NoAccountRow));
    assert!(lane.account_tier("binance", None).is_err());
}

#[test]
fn the_tier_is_the_account_rows() {
    let policy = vike_tradehub_core::AdapterPolicy::default().with_account(
        "binance",
        &AccountLabel::Default,
        AccountTier::Demo,
    );
    let lane = lane(&MIXED_REG, &policy);
    assert_eq!(lane.account_tier("binance", None), Ok(AccountTier::Demo));
}

/// **A row is followed by its id across a rename** (`row_of`, `row_label`): the row an account runs on is named by
/// its id, and after a rename that row carries its NEW label while the old label names no row. A paper tier,
/// another venue and an unknown id name nothing.
#[test]
fn a_row_is_followed_by_its_id_across_a_rename() {
    let row = |label: &str| vike_secrets::AccountRow {
        id: 2,
        venue: "binance".to_string(),
        tier: AccountTier::Demo,
        label: Some(label.to_string()),
        venue_account_id: None,
        parent_id: None,
        active: true,
        last_verified_at: None,
        max_exposure: None,
        leverage: None,
        symbol: vike_secrets::fixture_account_symbol("binance").map(str::to_string),
        facts: Default::default(),
    };
    let policy = |label| vike_tradehub_core::AdapterPolicy::default().with_account_row(row(label));
    let before = lane(&MIXED_REG, &policy("OLD"));
    assert_eq!(before.row_of("binance", Some("OLD"), AccountTier::Demo), Some(2));
    assert_eq!(before.row_of("binance", Some("OLD"), AccountTier::Paper), None);
    let after = lane(&MIXED_REG, &policy("NEW"));
    assert_eq!(after.account_tier("binance", Some("OLD")), Err(ArmingBlock::NoAccountRow));
    assert_eq!(after.row_label("binance", 2), Some(Some("NEW".to_string())));
    assert_eq!(after.account_tier("binance", Some("NEW")), Ok(AccountTier::Demo));
    assert_eq!(after.row_label("bybit", 2), None);
    assert_eq!(after.row_label("binance", 3), None);
}

/// A start on a venue this build does not carry is a refusal in the word of the block, never a paper stand-in.
#[test]
fn a_start_on_an_absent_or_unknown_venue_is_a_refusal_in_words() {
    let policy = vike_tradehub_core::AdapterPolicy::default();
    let lane = lane(&MIXED_REG, &policy);
    let absent = lane
        .start_adapter(&target("ibkr", AccountTier::Demo), &start(), events(), None)
        .expect_err("ibkr is not carried");
    assert_eq!(absent.block_word, super::arming::wire_word(ArmingBlock::FeatureAbsent));
    let unknown = lane
        .start_adapter(&target("nowhere", AccountTier::Demo), &start(), events(), None)
        .expect_err("nowhere is not a venue");
    assert_eq!(unknown.block_word, super::arming::wire_word(ArmingBlock::NoLiveArm));
}

/// **A (re)start of the account's adapter is a new lifetime for its kept leverage caps**
/// (`TradeLane::leverage_cap` keeps a confirmed figure until then): the next ask reads the venue again,
/// and an ask before the restart does not.
#[test]
fn a_start_forgets_the_accounts_kept_leverage_capabilities() {
    let policy = vike_tradehub_core::AdapterPolicy::default();
    let lane = lane(&MIXED_REG, &policy);
    let reads = std::cell::Cell::new(0);
    let ask = || {
        lane.leverage_capabilities_for_test()
            .confirmed("", AccountTier::Demo, "BTC", 10.0, || {
                reads.set(reads.get() + 1);
                Ok(10.0)
            })
            .expect("confirmed")
    };
    ask();
    ask();
    assert_eq!(reads.get(), 1, "a re-ask while the adapter lives reads nothing");
    // even a refused start is a new attempt: nothing kept from before it is trusted
    let _ = lane.start_adapter(&target("nowhere", AccountTier::Demo), &start(), events(), None);
    ask();
    assert_eq!(reads.get(), 2, "the next ask after a start reads the venue again");
}

/// An account that does not arm is NOT RUNNING with its block as a word AND a sentence an operator can act
/// on (the account row's remedy), not an empty string.
#[test]
fn a_refusal_carries_the_blocks_sentence() {
    let policy = vike_tradehub_core::AdapterPolicy::default().with_account(
        "binance",
        &AccountLabel::Default,
        AccountTier::Demo,
    );
    let lane = lane(&MIXED_REG, &policy);
    let refusal = lane
        .start_adapter(&target("binance", AccountTier::Demo), &start(), events(), None)
        .expect_err("no credentials, so the planted bridge resolves paper");
    assert_eq!(refusal.block_word, super::arming::wire_word(ArmingBlock::NoCredentials));
    assert!(refusal.refusal_text.contains("binance"), "{}", refusal.refusal_text);
    assert_ne!(refusal.refusal_text, refusal.block_word);
}

/// The first arming table of a process says what each bridge has to say about the accounts that will not
/// start, ONCE; the quiet table never does.
#[test]
fn the_not_running_report_is_said_once_per_process_by_the_first_arming_table() {
    let policy = vike_tradehub_core::AdapterPolicy::default().with_account(
        "bybit",
        &AccountLabel::Default,
        AccountTier::Demo,
    );
    let lane = lane(&SPEAKS_REG, &policy);
    let before = SPEAKS.spoke.load(Ordering::SeqCst);
    let _ = lane.arming_table_quiet();
    assert_eq!(SPEAKS.spoke.load(Ordering::SeqCst), before, "the quiet table says nothing");
    let _ = lane.arming_table();
    let once = SPEAKS.spoke.load(Ordering::SeqCst);
    assert_eq!(once, before + 1, "the first table speaks for the one account that will not start");
    let _ = lane.arming_table();
    assert_eq!(SPEAKS.spoke.load(Ordering::SeqCst), once, "…and only once");
}

/// A venue with a WIRED clock endpoint of this risk class (the planted bridge answers the read it is given).
const fn wired(risk: ClockRisk) -> VenueAdapterDeclaration {
    VenueAdapterDeclaration {
        clock: ClockDeclaration::Wired {
            endpoint: "planted",
            auth: ClockAuthentication::Public,
            risk,
        },
        ..PLANTED_DECLARATION
    }
}

static SIGNED: PlantedAdapter = PlantedAdapter {
    declaration: wired(ClockRisk::SignedTimestamp),
    server_time: Some(|_| Ok(1_700_000_000_123)),
    ..PlantedAdapter::new(
        "binance",
        ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials),
    )
};
static NONCE: PlantedAdapter = PlantedAdapter {
    declaration: wired(ClockRisk::NonceWindow),
    server_time: Some(|_| Err("timed out".to_string())),
    ..PlantedAdapter::new(
        "hyperliquid",
        ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials),
    )
};
static NO_CLOCK: PlantedAdapter = PlantedAdapter {
    declaration: VenueAdapterDeclaration {
        clock: ClockDeclaration::NotWired {
            reason: "ig publishes no clock",
            unmeasured_risk: None,
        },
        ..PLANTED_DECLARATION
    },
    ..PlantedAdapter::new("ig", ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials))
};
static RISKY: PlantedAdapter = PlantedAdapter {
    declaration: VenueAdapterDeclaration {
        clock: ClockDeclaration::NotWired {
            reason: "polymarket publishes no clock",
            unmeasured_risk: Some("its signature binds the timestamp into every order"),
        },
        ..PLANTED_DECLARATION
    },
    ..PlantedAdapter::new(
        "polymarket",
        ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials),
    )
};
static CLOCK_REG: [VenueAdapterRow; 4] = [
    VenueAdapterRow::Adapter(&SIGNED),
    VenueAdapterRow::Adapter(&NONCE),
    VenueAdapterRow::Adapter(&NO_CLOCK),
    VenueAdapterRow::Adapter(&RISKY),
];

fn clock_lane() -> TradeLaneBridge {
    let policy = ["binance", "hyperliquid", "ig", "polymarket"]
        .iter()
        .fold(vike_tradehub_core::AdapterPolicy::default(), |p, v| {
            p.with_account(v, &AccountLabel::Default, AccountTier::Demo)
        });
    lane(&CLOCK_REG, &policy)
}

fn venues_of(names: &[&str]) -> Vec<String> {
    names.iter().map(|v| (*v).to_string()).collect()
}

/// The batch preflight: nothing asked means nothing measured; otherwise one clock leg per venue (a venue
/// whose account states no tier is declared NotChecked, no read) and the network reading last.
#[test]
fn the_batch_preflight_has_a_clock_leg_per_venue_and_the_network_last() {
    let lane = lane(&MIXED_REG, &vike_tradehub_core::AdapterPolicy::default());
    assert!(lane.preflight(&[], PreflightStep::Batch).legs.is_empty());
    let legs = lane.preflight(&["binance".to_string()], PreflightStep::Batch).legs;
    assert!(
        matches!(&legs[0], PreflightLegWire::Clock { venue, leg: ServerTimeGapWire::NotChecked, reason: Some(why), .. }
            if venue == "binance" && why.contains("no account row states its tier")),
        "{legs:?}"
    );
    assert!(matches!(legs.last(), Some(PreflightLegWire::Network { .. })), "{legs:?}");
}

/// **The plan carries each wired venue's clock risk class WITH the bridge's remedy sentence**, the number
/// of wired reads (they buy time budget) and no measurement: a declared gap is not wired.
#[test]
fn the_plan_carries_the_clock_risk_and_its_remedy() {
    let lane = clock_lane();
    let all = venues_of(&["binance", "hyperliquid", "ig", "polymarket"]);
    let answer = lane.preflight(&all, PreflightStep::Plan);
    assert!(answer.legs.is_empty(), "a plan measures nothing");
    let lists = answer.plan.expect("the venue lists");
    assert_eq!(lists.wired_clock_reads, 2);
    assert_eq!(
        lists.clock_risks.len(),
        2,
        "only a WIRED venue has a risk: {:?}",
        lists.clock_risks
    );
    assert_eq!(lists.clock_risks["binance"].class, ClockRisk::SignedTimestamp);
    assert_eq!(lists.clock_risks["binance"].remedy, ClockRisk::SignedTimestamp.remedy());
    assert_eq!(lists.clock_risks["hyperliquid"].class, ClockRisk::NonceWindow);
    assert_eq!(lists.clock_risks["hyperliquid"].remedy, ClockRisk::NonceWindow.remedy());
    // The question names the venues: one that is not asked has no entry.
    let only =
        lane.preflight(&venues_of(&["hyperliquid"]), PreflightStep::Plan).plan.expect("lists");
    assert_eq!(only.wired_clock_reads, 1);
    assert!(
        only.clock_risks.contains_key("hyperliquid") && !only.clock_risks.contains_key("binance")
    );
}

/// **A clock question is ONE read per venue named**: a stamp for a wired venue, the venue's error for one
/// that did not answer, and the row's own sentence (and what a drifted clock costs) for a declared gap.
#[test]
fn a_clock_question_is_one_read_and_a_declared_gap_states_its_reason() {
    let lane = clock_lane();
    let legs = lane
        .preflight(
            &venues_of(&["binance", "hyperliquid", "ig", "polymarket"]),
            PreflightStep::Clock,
        )
        .legs;
    assert_eq!(legs.len(), 4, "one leg per venue named, no network, no credential: {legs:?}");
    assert!(matches!(
        &legs[0],
        PreflightLegWire::Clock { venue, leg: ServerTimeGapWire::ServerTime { server_ms: 1_700_000_000_123, .. }, reason: None, .. }
            if venue == "binance"
    ));
    assert!(matches!(
        &legs[1],
        PreflightLegWire::Clock { leg: ServerTimeGapWire::Unreachable(why), .. } if why.contains("timed out")
    ));
    assert!(matches!(
        &legs[2],
        PreflightLegWire::Clock { leg: ServerTimeGapWire::NotChecked, reason: Some(why), at_stake: None, .. }
            if why == "ig publishes no clock"
    ));
    assert!(matches!(
        &legs[3],
        PreflightLegWire::Clock { leg: ServerTimeGapWire::UnmeasuredRisk, reason: Some(why), at_stake: Some(cost), .. }
            if why == "polymarket publishes no clock"
                && cost == "its signature binds the timestamp into every order"
    ));
}

/// A credential question asked BEFORE any plan builds the probes first: the answer is the probe table's own
/// (here a venue that would go live and offers no probe), never "no credential probe was planned", which is
/// what an unbuilt table says.
#[test]
fn a_credential_question_before_any_plan_builds_its_probes() {
    let lane = clock_lane();
    let legs = lane.preflight(&venues_of(&["binance"]), PreflightStep::Credential).legs;
    assert!(
        matches!(&legs[..], [PreflightLegWire::Credential { venue, label: None, leg: CredentialGapWire::Rejected(why) }]
            if venue == "binance" && !why.contains("was planned")),
        "{legs:?}"
    );
}

/// The network question answers ONE network leg whatever venues it names.
#[test]
fn a_network_question_answers_one_network_leg() {
    let legs = clock_lane().preflight(&[], PreflightStep::Network).legs;
    assert!(matches!(&legs[..], [PreflightLegWire::Network { .. }]), "{legs:?}");
}

/// **The wire's network leg carries the numbers the report prints**, and the clock and credential legs
/// carry what the originals said (one mapping, shared by the lane and every scripted double).
#[test]
fn the_wire_legs_carry_the_probes_words_and_numbers() {
    assert_eq!(
        network_result(NetReading::Alive { rounds: Some(7) }),
        PreflightLegWire::Network {
            reading: NetReadingWire::Alive,
            rounds: Some(7),
            since_ms: None
        }
    );
    assert!(matches!(
        network_result(NetReading::Dead { since_ms: 42 }),
        PreflightLegWire::Network {
            reading: NetReadingWire::Dead(_),
            rounds: None,
            since_ms: Some(42)
        }
    ));
    assert_eq!(
        network_result(NetReading::Unknown),
        PreflightLegWire::Network {
            reading: NetReadingWire::Unknown,
            rounds: None,
            since_ms: None
        }
    );
    assert_eq!(
        credential_result(
            "okx",
            None,
            Err(CredentialGap::Unanswered { waited_ms: 5, detail: "d".into() })
        ),
        PreflightLegWire::Credential {
            venue: "okx".into(),
            label: None,
            leg: CredentialGapWire::Unanswered { waited_ms: 5, detail: "d".into() }
        }
    );
    assert_eq!(
        clock_result("binance", Ok(10), 12, 4),
        PreflightLegWire::Clock {
            venue: "binance".into(),
            leg: ServerTimeGapWire::ServerTime { server_ms: 10, local_ms: 12, rtt_ms: 4 },
            reason: None,
            at_stake: None
        }
    );
}

/// A policy-less table for the directory the lane was built from: the snapshot never carries a stop.
#[test]
fn the_lane_drops_a_preflight_stop_from_its_snapshot() {
    let mut policy = vike_tradehub_core::AdapterPolicy::default().with_account(
        "binance",
        &AccountLabel::Default,
        AccountTier::Demo,
    );
    policy.preflight_failed.insert("binance".to_string());
    let lane = lane(&MIXED_REG, &policy);
    let table = lane.arming_table_quiet();
    let row = table.rows.iter().find(|r| r.venue == "binance").expect("the account's row");
    assert_ne!(
        row.block_word,
        super::arming::wire_word(ArmingBlock::PreflightFailed),
        "the stop is the tradehub's overlay over the table, never the daemon's state"
    );
}

#[test]
fn the_constructor_over_a_directory_matches_the_snapshot_one() {
    let policy = vike_tradehub_core::AdapterPolicy::default().with_account(
        "binance",
        &AccountLabel::Default,
        AccountTier::Live,
    );
    let a = TradeLaneBridge::new(&MIXED_REG, HashMap::new(), &policy.accounts, &BTreeMap::new());
    let b = lane(&MIXED_REG, &policy);
    let (mut a, mut b) = (a.arming_table_quiet(), b.arming_table_quiet());
    assert!(a.start_inputs.keys().eq(b.start_inputs.keys()), "both count the same accounts");
    // Each lane counts from its own draw (`StartedKeys::start_inputs`): a counter compares only with itself.
    a.start_inputs.clear();
    b.start_inputs.clear();
    assert_eq!(a, b);
}

// ---- the supervised link (decision 0164 verdict 7(b)) -------------------------------------------------------

mod link_watch {
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use vike_secrets::account_directory::AccountDirectory;
    use vike_secrets::venue_setting::VenueSettings;
    use vike_secrets::{AccountRow, AccountRows, VenueSettingRow};

    use super::*;

    fn ibkr_row(id: i64, tier: AccountTier, active: bool) -> AccountRow {
        AccountRow {
            id,
            venue: "ibkr".to_string(),
            tier,
            label: None,
            venue_account_id: None,
            parent_id: None,
            active,
            last_verified_at: None,
            max_exposure: None,
            leverage: None,
            symbol: vike_secrets::fixture_account_symbol("ibkr").map(str::to_string),
            facts: Default::default(),
        }
    }

    fn directory(rows: Vec<AccountRow>) -> AccountDirectory {
        AccountDirectory::from_rows(AccountRows::Known(rows), None)
    }

    /// The demo tier's gateway ADDRESS, a loopback socket at `port` (the `venue_url` row, decision 0265).
    fn gateway_rows(port: u16) -> BTreeMap<String, VenueSettings> {
        use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlType, VenueUrls};
        let socket = format!("127.0.0.1:{port}");
        let row = (
            "ibkr",
            UrlType::Demo,
            UrlChannel::Trade,
            UrlProtocol::Socket,
            "gateway",
            socket.as_str(),
        );
        let urls = VenueUrls::from_rows("ibkr", [row]);
        BTreeMap::from([("ibkr".to_string(), VenueSettings::default().with_urls(urls))])
    }

    fn lane_over(
        accounts: &AccountDirectory,
        settings: &BTreeMap<String, VenueSettings>,
    ) -> TradeLaneBridge {
        TradeLaneBridge::new(&super::SPEAKS_REG, HashMap::new(), accounts, settings)
    }

    /// The PRODUCTION table over `accounts` and `settings`: a supervised link is each registry bridge's own declaration
    /// (`VenueAdapter::link_probe`), so only the real rows can answer `link_watch`.
    fn supervising_lane(
        accounts: &AccountDirectory,
        settings: &BTreeMap<String, VenueSettings>,
    ) -> TradeLaneBridge {
        TradeLaneBridge::new(crate::registry::trade::REGISTRY, HashMap::new(), accounts, settings)
    }

    /// The venues whose bridge declares its session supervised, in a build that carries their adapter.
    fn supervised() -> Vec<&'static str> {
        [cfg!(feature = "ibkr").then_some("ibkr"), cfg!(feature = "fxcm").then_some("fxcm")]
            .into_iter()
            .flatten()
            .collect()
    }

    #[cfg(any(feature = "fxcm", feature = "ibkr"))]
    fn saturday() -> i64 {
        vike_model::time::parse_utc_timestamp_ms("2026-11-21T12:00:00Z").expect("an instant")
    }

    #[cfg(feature = "fxcm")]
    fn wednesday() -> i64 {
        vike_model::time::parse_utc_timestamp_ms("2026-11-18T12:00:00Z").expect("an instant")
    }

    /// Only a bridge that DECLARES its session supervised is given a watch (decision 0164 verdict 7(b), amended
    /// 2026-11-21: `VenueAdapter::link_probe`, never a venue name in the daemon): IBKR's gateway and FXCM's ForexConnect
    /// session. Every other bridge owns its reconnect, and the daemon adds none.
    #[test]
    fn only_the_bridges_that_declare_a_probe_are_supervised() {
        let lane = supervising_lane(&directory(vec![]), &gateway_rows(1));
        assert!(lane.link_watch(&target("binance", AccountTier::Demo), &start()).is_none());
        for venue in supervised() {
            assert!(
                lane.link_watch(&target(venue, AccountTier::Demo), &start()).is_some(),
                "{venue}"
            );
        }
    }

    /// The two seams are separate: the daemon SUPERVISES only the sessions a bridge declares (`link_watch`: IBKR's
    /// gateway, FXCM's ForexConnect session), and every venue whose connection row reports a path (all of the roster:
    /// `vike_venues::venue_connection`'s `path_report`) is REPORTED (`venue_link`, a reading that rebuilds nothing) off
    /// the ONE cell its bridge writes, `vike_bridge_core::venue_link::venue_link`, found by the start's route key. A cell
    /// nobody wrote to is not known (no report, never up), and another account's cell is its own. IBKR and FXCM are
    /// both: the daemon supervises the gateway's port and the SDK's final states, and each bridge reports its own path.
    /// That each bridge WRITES the cell is its own tests' (and `venue_connection_sites_gate`'s).
    #[test]
    fn the_venues_that_report_a_path_are_not_supervised_and_the_supervised_ones_report_theirs() {
        use vike_bridge_core::venue_link::{VenueLinkState, venue_link};
        let lane = lane_over(&directory(vec![]), &gateway_rows(1));
        let supervising = supervising_lane(&directory(vec![]), &gateway_rows(1));
        for &venue in vike_model::VENUES {
            let route = format!("{venue}#SLINKONE");
            let mine = AdapterStart { route_key: route.clone(), ..start() };
            let reporting = target(venue, AccountTier::Demo);
            let report = lane.venue_link(&reporting, &mine).expect("every roster venue reports");
            assert_eq!(report(), None, "{venue}: nothing said yet is not up");
            venue_link(&route).set(VenueLinkState::Up, None);
            assert_eq!(report(), Some(VenueLinkWire::working()), "{venue}: the cell's word");
            let theirs = AdapterStart { route_key: format!("{venue}#SLINKTWO"), ..start() };
            let other = lane.venue_link(&reporting, &theirs).expect("reports");
            assert_eq!(other(), None, "{venue}: another account's cell is its own");
            assert_eq!(
                supervising.link_watch(&reporting, &start()).is_some(),
                supervised().contains(&venue),
                "{venue}: only a declared session is supervised; every other bridge heals its path itself"
            );
        }
        let stranger = target("sim", AccountTier::Demo);
        assert!(lane.venue_link(&stranger, &start()).is_none(), "an unknown venue reports nothing");
    }

    /// **An ibkr account on the socket backend has IB hold its time release; one on the Client Portal does not**
    /// (decision 0267's 2.6, `TradeLane::native_time_release`): read off the tier's `backend` row as a start resolves
    /// it (no row is the socket), an account with no `ACCOUNT` field resolves nothing and answers `false`, and so
    /// does every other venue.
    #[cfg(feature = "ibkr")]
    #[test]
    fn an_ibkr_socket_account_holds_its_time_release_at_ibkr_and_a_client_portal_one_does_not() {
        let backend = |word: Option<&str>| {
            let rows: Vec<VenueSettingRow> = word
                .map(|w| VenueSettingRow {
                    venue: "ibkr".into(),
                    tier: Some(AccountTier::Demo),
                    field: "backend".into(),
                    value: w.into(),
                })
                .into_iter()
                .collect();
            // The tier's `backend` row beside the release's gateway addresses (decision 0265).
            let settings = VenueSettings::from_rows("ibkr", &rows)
                .with_urls(vike_venues::venue_urls::VenueUrls::seed("ibkr"));
            BTreeMap::from([("ibkr".to_string(), settings)])
        };
        let mut book = vike_secrets::account_credentials::CredentialRegistry::new();
        book.insert_account(1, "ACCOUNT", "DU1234567");
        let held = directory(vec![ibkr_row(1, AccountTier::Demo, true)]).with_credentials(book);
        let lane = |accounts: &AccountDirectory, word: Option<&str>| {
            TradeLaneBridge::new(&super::IBKR_REG, HashMap::new(), accounts, &backend(word))
        };
        let demo = target("ibkr", AccountTier::Demo);
        for (word, native) in [(None, true), (Some("socket"), true), (Some("cpapi"), false)] {
            assert_eq!(lane(&held, word).native_time_release(&demo, &start()), native, "{word:?}");
        }
        let bare = directory(vec![ibkr_row(1, AccountTier::Demo, true)]);
        assert!(!lane(&bare, None).native_time_release(&demo, &start()), "no ACCOUNT field");
        let other = target("binance", AccountTier::Demo);
        assert!(!lane(&held, None).native_time_release(&other, &start()), "another venue");
    }

    /// The watch probes the port the tier's rows name and asks the rows AGAIN at every call, through the
    /// fresh-read source when the daemon built one.
    #[cfg(feature = "ibkr")]
    #[test]
    fn the_watch_probes_the_tiers_port_and_reads_the_rows_fresh() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("port").port();
        let active = Arc::new(AtomicBool::new(true));
        let reads = Arc::new(AtomicUsize::new(0));
        let rows: super::super::bridge_trade::AccountDirectorySource = {
            let (active, reads) = (Arc::clone(&active), Arc::clone(&reads));
            Arc::new(move || {
                reads.fetch_add(1, Ordering::SeqCst);
                directory(vec![ibkr_row(1, AccountTier::Demo, active.load(Ordering::SeqCst))])
            })
        };
        // The snapshot the lane started with says the account is active and never changes.
        let started = directory(vec![ibkr_row(1, AccountTier::Demo, true)]);
        let lane = supervising_lane(&started, &gateway_rows(port)).with_account_rows(rows);
        let watch =
            lane.link_watch(&target("ibkr", AccountTier::Demo), &start()).expect("ibkr is watched");

        assert_eq!(watch.reachable(), Ok(()), "the fake gateway answers");
        assert_eq!(watch.tier_now(), Ok(AccountTier::Demo));
        active.store(false, Ordering::SeqCst);
        assert_eq!(
            watch.tier_now(),
            Err(ArmingBlock::AccountInactive),
            "an `active = 0` written after the start is what the next call sees"
        );
        assert_eq!(reads.load(Ordering::SeqCst), 2, "each call re-read the rows");
        drop(listener);
        assert!(watch.reachable().is_err(), "the closed port is a loss");
    }

    /// Without a fresh source the predicate answers from the start's snapshot: it never invents a change.
    #[cfg(feature = "ibkr")]
    #[test]
    fn without_a_fresh_source_the_snapshot_answers() {
        let started = directory(vec![ibkr_row(1, AccountTier::Demo, true)]);
        let lane = supervising_lane(&started, &gateway_rows(1));
        let watch = lane.link_watch(&target("ibkr", AccountTier::Demo), &start()).expect("watched");
        assert_eq!(watch.tier_now(), Ok(AccountTier::Demo));
        // The bridge's cadence (`vike_ibkr::gateway_watch`) is a schedule the reconnect loop runs on.
        assert!(watch.schedule().is_sane());
        assert_eq!(
            watch.schedule().first_wait,
            vike_venues::venue_timings::IBKR.exec().reconnect.unwrap().initial
        );
        // The parked words are the bridge's own.
        assert_eq!(watch.not_ready_words(), vike_ibkr::gateway_watch::GATEWAY_NOT_LOGGED_IN);
    }

    /// The venue-scoped two-tier conflict of decision 0140 (a demo row beside a live row) is read through
    /// `account_tier`, so it stops the watch's predicate exactly as it stops an arm.
    #[cfg(feature = "ibkr")]
    #[test]
    fn the_two_tier_conflict_is_the_predicates_answer() {
        let rows = directory(vec![
            ibkr_row(1, AccountTier::Demo, true),
            ibkr_row(2, AccountTier::Live, true),
        ]);
        let lane = supervising_lane(&rows, &gateway_rows(1));
        let watch = lane.link_watch(&target("ibkr", AccountTier::Demo), &start()).expect("watched");
        assert_eq!(watch.tier_now(), Err(ArmingBlock::TierConflict));
    }

    /// `oauth` is a backend nothing connects through: there is no gateway to watch.
    #[test]
    fn an_oauth_backend_has_no_gateway_to_watch() {
        let rows = vec![VenueSettingRow {
            venue: "ibkr".into(),
            tier: Some(AccountTier::Demo),
            field: "BACKEND".into(),
            value: "oauth".into(),
        }];
        let settings =
            BTreeMap::from([("ibkr".to_string(), VenueSettings::from_rows("ibkr", &rows))]);
        let lane = supervising_lane(&directory(vec![]), &settings);
        assert!(lane.link_watch(&target("ibkr", AccountTier::Demo), &start()).is_none());
    }

    /// **FXCM's watch is its bridge's probe over the account's OWN cell** (by the start's route key), and its rebuild is
    /// held to the FX week on the lane's clock: a final SDK state is the loss, a Saturday names Sunday's open, a Wednesday
    /// and a lane with no clock hold nothing. A refused rebuild waits as a failed FXCM login does, and a parked start
    /// says the daemon's own words.
    #[cfg(feature = "fxcm")]
    #[test]
    fn the_fxcm_watch_reads_its_accounts_cell_and_holds_a_rebuild_to_the_fx_week() {
        use vike_bridge_core::venue_link::venue_link;
        let mine = AdapterStart { route_key: "fxcm#WATCHFX".to_string(), ..start() };
        let fxcm = target("fxcm", AccountTier::Demo);
        let rows = directory(vec![]);
        let held = supervising_lane(&rows, &gateway_rows(1)).with_fx_clock(Arc::new(saturday));
        let watch = held.link_watch(&fxcm, &mine).expect("fxcm's session is supervised");
        assert_eq!(watch.lost(), None, "nothing said yet");
        venue_link("fxcm#WATCHFX").set(vike_fxcm::account::venue_link::from_status(Some(4)), None);
        assert_eq!(watch.lost(), None, "the SDK's own reconnect is never raced");
        venue_link("fxcm#WATCHFX").set(vike_fxcm::account::venue_link::from_status(Some(6)), None);
        assert_eq!(watch.lost().as_deref(), Some("the ForexConnect session is SessionLost"));
        assert_eq!(watch.reachable(), Ok(()));
        assert_eq!(
            watch.login_closed().as_deref(),
            Some("FX market closed: login at 2026-11-22T21:00:00Z"),
            "a Saturday holds the rebuild to Sunday's open"
        );
        assert!(watch.schedule().is_sane());
        assert_eq!(watch.schedule().first_wait, std::time::Duration::from_secs(30));
        assert_eq!(watch.not_ready_words(), crate::trade::lane::NOT_READY_WORDS);

        let open = supervising_lane(&rows, &gateway_rows(1)).with_fx_clock(Arc::new(wednesday));
        let watch = open.link_watch(&fxcm, &mine).expect("supervised");
        assert_eq!(watch.login_closed(), None, "a Wednesday holds nothing");
        let unclocked = supervising_lane(&rows, &gateway_rows(1));
        let watch = unclocked.link_watch(&fxcm, &mine).expect("supervised");
        assert_eq!(watch.login_closed(), None, "a lane with no clock holds nothing, as its starts");
    }

    /// **IBKR's supervision is never held by the FX week** (its book is not the FX week's: `session_for` answers it
    /// always open), on the same Saturday clock: the move to the declared verb changes nothing for it.
    #[cfg(feature = "ibkr")]
    #[test]
    fn the_ibkr_watch_is_never_held_by_the_fx_week() {
        let lane = supervising_lane(&directory(vec![]), &gateway_rows(1))
            .with_fx_clock(Arc::new(saturday));
        let watch = lane.link_watch(&target("ibkr", AccountTier::Demo), &start()).expect("watched");
        assert_eq!(watch.login_closed(), None);
        assert_eq!(watch.lost(), None, "the bridge reports its own death; the probe is the port");
    }
}

/// An armed venue whose `start` counts the times its bridge was asked, and comes up empty.
struct Counting {
    started: AtomicUsize,
}

impl VenueAdapter for Counting {
    fn venue(&self) -> &'static str {
        "ig"
    }
    fn declaration(&self) -> VenueAdapterDeclaration {
        VenueAdapterDeclaration { addresses_accounts: true, ..PLANTED_DECLARATION }
    }
    fn resolve(&self, _inputs: &AdapterInputs<'_>) -> ArmingResolution {
        ArmingResolution::Armed { tier: AccountTier::Demo, held_below_live: None }
    }
    fn start(&self, _req: AdapterRequest<'_>) -> AdapterOutcome {
        self.started.fetch_add(1, Ordering::SeqCst);
        AdapterOutcome::paper()
    }
}

static COUNTING: Counting = Counting { started: AtomicUsize::new(0) };
static COUNTING_REG: [VenueAdapterRow; 1] = [VenueAdapterRow::Adapter(&COUNTING)];

/// **A start on a snapshot whose `venue_setting` rows could not be read is refused in words and never
/// reaches the bridge** (review 15, finding 6): the empty map such a read leaves is not "no rows". The
/// control: the same lane over a snapshot that read its table reaches the bridge's `start`.
#[test]
fn a_start_on_unread_venue_settings_is_refused_and_the_bridge_is_not_asked() {
    let policy = vike_tradehub_core::AdapterPolicy::default().with_account(
        "ig",
        &AccountLabel::Default,
        AccountTier::Demo,
    );
    let ig = |tier| target("ig", tier);

    let before = COUNTING.started.load(Ordering::SeqCst);
    let read = lane(&COUNTING_REG, &policy);
    let _ = read.start_adapter(&ig(AccountTier::Demo), &start(), events(), None);
    assert_eq!(
        COUNTING.started.load(Ordering::SeqCst),
        before + 1,
        "control: a table that was read reaches the bridge"
    );

    let mut snapshot = super::testutil::snapshot_of(&policy);
    snapshot.venue_settings_unread = Some("the settings database is locked".to_string());
    let unread = TradeLaneBridge::with_snapshot(&COUNTING_REG, HashMap::new(), snapshot);
    let refusal = unread
        .start_adapter(&ig(AccountTier::Demo), &start(), events(), None)
        .expect_err("a start on defaults the operator did not choose is refused");
    assert_eq!(refusal.block_word, super::arming::wire_word(ArmingBlock::AdapterFailed));
    assert!(
        refusal.refusal_text.contains("venue_setting") && refusal.refusal_text.contains("locked"),
        "the reason is named: {}",
        refusal.refusal_text
    );
    assert_eq!(
        COUNTING.started.load(Ordering::SeqCst),
        before + 1,
        "the bridge was not asked to start on built-in defaults"
    );
}

static ALPACA: PlantedAdapter = PlantedAdapter::new(
    "alpaca",
    ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials),
);
static ALPACA_REG: [VenueAdapterRow; 1] = [VenueAdapterRow::Adapter(&ALPACA)];

/// A lane whose PUBLIC list is a trap: alpaca's facts must never be asked of it. Its addresses are the
/// release seed's, as a seeded box's are.
/// ig's refusal of an account with no credentials, whether it asks plainly or converting:
/// `TradeLaneBridge::ig_account_source`'s words, before any login.
const IG_HOLDS_NO_CREDENTIALS: &str = "ig: this account holds no usable credentials (API_KEY, \
     IDENTIFIER and PASSWORD), so no epic is looked up for it";

fn facts_lane(registry: &'static [VenueAdapterRow]) -> TradeLaneBridge {
    use super::symbol_facts::SymbolPropertiesCache;
    let policy = vike_tradehub_core::AdapterPolicy {
        venue_settings: vike_secrets::venue_setting::seeded_venue_settings(),
        ..vike_tradehub_core::AdapterPolicy::default()
    };
    lane(registry, &policy).with_symbol_facts(std::sync::Arc::new(SymbolPropertiesCache::new(
        std::sync::Arc::new(|venue| Err(format!("{venue}: the public list must not be asked for"))),
    )))
}

/// **alpaca's facts are the account's own listing, never the public one**: a `live` account is refused
/// (the adapter trades its sandbox only), an account with no credentials lists nothing and says so (no
/// request leaves), and a build with no alpaca row answers the default.
#[test]
fn alpaca_symbol_facts_come_from_the_accounts_own_listing() {
    let lane = facts_lane(&ALPACA_REG);
    let live = lane.symbol_facts(&target("alpaca", AccountTier::Live), "AAPL").expect_err("live");
    assert_eq!(live.code, SymbolPropertiesRefusalCode::Verdict, "{live}");
    assert!(live.reason.contains("sandbox only"), "{live}");
    let none = lane.symbol_facts(&target("alpaca", AccountTier::Demo), "AAPL").expect_err("no key");
    assert_eq!(
        none.reason,
        format!(
            "alpaca: {PROPERTIES_LIST_UNAVAILABLE}: alpaca: this account holds no complete sandbox \
             credentials (CLIENT_ID, CLIENT_SECRET and ACCOUNT_ID)"
        ),
        "no credentials: refused by field name, asked of nothing"
    );
    assert_eq!(
        none.code,
        SymbolPropertiesRefusalCode::ListUnavailable,
        "a missing credential is not a verdict on the symbol: {none}"
    );
    let absent = facts_lane(&SPEAKS_REG)
        .symbol_facts(&target("alpaca", AccountTier::Demo), "AAPL")
        .expect_err("no alpaca row");
    assert_eq!(absent, super::symbol_facts::verdict(super::symbol_facts::NOT_ANSWERED));
    // Another venue still reads the public list (the trap above answers for it).
    let other =
        lane.symbol_facts(&target("bybit", AccountTier::Demo), "ETHUSDT").expect_err("trap");
    assert!(other.reason.contains("public list must not be asked for"), "{other}");
}

/// **hyperliquid's facts are the ASKING account's network's universe**: a `demo` account (testnet) is asked for the
/// testnet listing and a `live` one for mainnet's, through the same lane, so a coin only mainnet lists is refused
/// at the door for the testnet account (no extra-symbol slot spent) and the same coin is sized on each network's
/// own grid. Every other venue still reads the one public list.
#[test]
fn hyperliquid_symbol_facts_follow_the_accounts_network() {
    use super::symbol_facts::SymbolPropertiesCache;
    // Asked by USER name (decision 0290): the kept rows say which listing row a name is.
    crate::testkit::fixtures::hold_hyperliquid_kept_rows();
    use vike_hyperliquid::symbol_properties::HYPERLIQUID_TESTNET_LIST;
    use vike_model::{Contract, ContractPerp, Instrument, Margining, SymbolProperties};
    let perp = |coin: &str, tick: f64| {
        Instrument::new(
            "hyperliquid",
            coin,
            coin,
            "USDC",
            Contract::Perp(ContractPerp {
                margining: Margining::Linear,
                settle: "USDC".into(),
                value_currency: None,
                funding_interval_ms: Some(3_600_000),
                settling_ms: None,
            }),
        )
        .with_properties(SymbolProperties {
            tick_size: tick,
            step_size: 0.001,
            min_qty: 0.001,
            min_notional: 10.0,
            ..Default::default()
        })
    };
    let (mainnet, testnet) = (vec![perp("BTC", 0.1), perp("XRP", 0.0001)], vec![perp("BTC", 1.0)]);
    let lane = lane(&ALPACA_REG, &vike_tradehub_core::AdapterPolicy::default()).with_symbol_facts(
        std::sync::Arc::new(SymbolPropertiesCache::new(std::sync::Arc::new(
            move |key| match key {
                "hyperliquid" => Ok(mainnet.clone()),
                k if k == HYPERLIQUID_TESTNET_LIST => Ok(testnet.clone()),
                other => Err(format!("{other}: the public list must not be asked for")),
            },
        ))),
    );

    // The testnet account: XRP is not listed there, BTC is, on the testnet's tick.
    let demo = target("hyperliquid", AccountTier::Demo);
    let why = lane.symbol_facts(&demo, "XRPUSDC.P").expect_err("not on the testnet");
    assert_eq!(why.code, SymbolPropertiesRefusalCode::NoInstrument, "{why}");
    assert!(
        why.reason.contains("no instrument XRPUSDC.P") && why.reason.contains("testnet"),
        "{why}"
    );
    assert_eq!(lane.symbol_facts(&demo, "BTCUSDC.P").expect("BTC").properties.tick_size, 1.0);

    // The mainnet account: the mainnet universe, XRP included, BTC on mainnet's tick.
    let live = target("hyperliquid", AccountTier::Live);
    assert!(lane.symbol_facts(&live, "XRPUSDC.P").is_ok());
    assert_eq!(lane.symbol_facts(&live, "BTCUSDC.P").expect("BTC").properties.tick_size, 0.1);

    // A quoted ask (a node that converts) takes the same road for a venue with no rate source of its own.
    assert_eq!(
        lane.symbol_facts_quoted(&demo, "BTCUSDC.P").expect("BTC").properties.tick_size,
        1.0
    );
}

/// **okx's facts are the ASKING account's network's list too**: demo and mainnet share a host and the demo is a
/// header, and the two lists differ (the USDT dated futures exist on the demo alone). A `demo` account is asked
/// of the simulated-trading list and a `live` one of mainnet's, through the same lane, for a dated future (one
/// symbol at a time) and for a swap (the bulk list), so a demo-only future is admitted for the demo account and
/// refused for the live one.
#[test]
fn okx_symbol_facts_follow_the_accounts_network() {
    use super::symbol_facts::SymbolPropertiesCache;
    use vike_okx::symbol_properties::OKX_DEMO_LIST;
    let row = |instrument: &str, inst_type: &str, expires: bool| {
        serde_json::json!({
            "instType": inst_type, "instId": instrument, "uly": "QTUM-USDT", "instFamily": "QTUM-USDT",
            "baseCcy": "", "quoteCcy": "", "settleCcy": "USDT", "ctVal": "10", "ctMult": "1",
            "ctValCcy": "QTUM", "ctType": "linear", "alias": "",
            "expTime": if expires { (vike_model::now_ms() + 90 * 86_400_000).to_string() } else { String::new() },
            "tickSz": "0.001", "lotSz": "1", "minSz": "1", "maxMktSz": "1000", "state": "live",
        })
    };
    let rows = |list: &[(&str, &str, bool)]| {
        vike_okx::catalog::parse_instruments(&serde_json::json!({
            "data": list.iter().map(|(i, t, e)| row(i, t, *e)).collect::<Vec<_>>()
        }))
    };
    let mainnet = rows(&[("QTUM-USDT-SWAP", "SWAP", false), ("OLD-USDT-SWAP", "SWAP", false)]);
    let demo = rows(&[("QTUM-USDT-SWAP", "SWAP", false), ("QTUM-USDT-261106", "FUTURES", true)]);
    let (bulk_main, bulk_demo) = (mainnet.clone(), demo.clone());
    let list: super::symbol_facts::InstrumentList = std::sync::Arc::new(move |key| match key {
        "okx" => Ok(bulk_main.clone()),
        k if k == OKX_DEMO_LIST => Ok(bulk_demo.clone()),
        other => Err(format!("{other}: the public list must not be asked for")),
    });
    let one: super::symbol_facts::SymbolLister = std::sync::Arc::new(move |key, symbol| {
        let from = if key == OKX_DEMO_LIST { &demo } else { &mainnet };
        Ok(from.iter().filter(|r| r.raw_symbol == symbol).cloned().collect())
    });
    let lane = lane(&ALPACA_REG, &vike_tradehub_core::AdapterPolicy::default()).with_symbol_facts(
        std::sync::Arc::new(SymbolPropertiesCache::new(list).with_symbol_lister(one)),
    );

    let demo = target("okx", AccountTier::Demo);
    assert!(lane.symbol_facts(&demo, "QTUM-USDT-261106").is_ok(), "the demo-only future");
    assert!(lane.symbol_facts(&demo, "QTUM-USDT-SWAP").is_ok());
    let why = lane.symbol_facts(&demo, "OLD-USDT-SWAP").expect_err("mainnet-only swap");
    assert!(
        why.reason.contains("no instrument OLD-USDT-SWAP")
            && why.reason.contains("simulated-trading"),
        "{why}"
    );

    let live = target("okx", AccountTier::Live);
    let why = lane.symbol_facts(&live, "QTUM-USDT-261106").expect_err("not on mainnet");
    assert!(why.reason.contains("no instrument QTUM-USDT-261106"), "{why}");
    assert!(lane.symbol_facts(&live, "OLD-USDT-SWAP").is_ok(), "mainnet's own swap");
}

static CTRADER: PlantedAdapter = PlantedAdapter::new(
    "ctrader",
    ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials),
);
static CTRADER_REG: [VenueAdapterRow; 1] = [VenueAdapterRow::Adapter(&CTRADER)];

/// **ctrader's facts are the account's own session listing, never the public one**: a `live` or `paper`
/// account is refused (the adapter dials the demo host alone), an account with no credentials lists
/// nothing and says so (no session opens), and a build with no ctrader row answers the default.
#[test]
fn ctrader_symbol_facts_come_from_the_accounts_own_session() {
    let lane = facts_lane(&CTRADER_REG);
    for tier in [AccountTier::Live, AccountTier::Paper] {
        let why = lane.symbol_facts(&target("ctrader", tier), "EURUSD").expect_err("not demo");
        assert!(why.reason.contains("demo account only"), "{tier:?}: {why}");
    }
    let none =
        lane.symbol_facts(&target("ctrader", AccountTier::Demo), "EURUSD").expect_err("no key");
    assert_eq!(
        none,
        super::symbol_facts::coded(
            SymbolPropertiesRefusalCode::ListUnavailable,
            format!(
                "ctrader: {PROPERTIES_LIST_UNAVAILABLE}: ctrader: this account holds no usable \
                 credentials (the OAuth grant and the application pair)"
            )
        ),
        "no credentials: nothing to authenticate with, the public list is never asked"
    );
    let absent = facts_lane(&SPEAKS_REG)
        .symbol_facts(&target("ctrader", AccountTier::Demo), "EURUSD")
        .expect_err("no ctrader row");
    assert_eq!(absent, super::symbol_facts::verdict(super::symbol_facts::NOT_ANSWERED));
}

static OANDA: PlantedAdapter = PlantedAdapter::new(
    "oanda",
    ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials),
);
static OANDA_REG: [VenueAdapterRow; 1] = [VenueAdapterRow::Adapter(&OANDA)];

/// **oanda's facts are the account's own listing too, for a `demo` account only**: a `live` or `paper`
/// account is refused (the adapter trades its practice account alone), an account with no credentials is
/// refused in words before any request is built (an unknown currency is never "any currency"), and a build
/// with no oanda row answers the default. The public list (the trap) is never asked.
#[test]
fn oanda_symbol_facts_come_from_the_accounts_own_listing() {
    let lane = facts_lane(&OANDA_REG);
    for tier in [AccountTier::Live, AccountTier::Paper] {
        let why = lane.symbol_facts(&target("oanda", tier), "GBPUSD").expect_err("not demo");
        assert!(why.reason.contains("practice account only"), "{tier:?}: {why}");
    }
    let none =
        lane.symbol_facts(&target("oanda", AccountTier::Demo), "GBPUSD").expect_err("no key");
    assert_eq!(none.code, SymbolPropertiesRefusalCode::ListUnavailable, "{none}");
    assert_eq!(
        none.reason,
        format!(
            "oanda: {PROPERTIES_LIST_UNAVAILABLE}: oanda: this account holds no usable credentials \
             (API_KEY and ACCOUNT_ID)"
        ),
        "no credentials: refused before a request, asked of nothing"
    );
    let absent = facts_lane(&SPEAKS_REG)
        .symbol_facts(&target("oanda", AccountTier::Demo), "GBPUSD")
        .expect_err("no oanda row");
    assert_eq!(absent, super::symbol_facts::verdict(super::symbol_facts::NOT_ANSWERED));
}

/// A lane holding ONE oanda demo account (row 1, `active` as given) with its practice keys, on the address `base`.
fn oanda_lane(base: &str, active: bool) -> TradeLaneBridge {
    use vike_secrets::{AccountRow, AccountRows};
    use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlType, VenueUrls};

    let urls = VenueUrls::from_rows(
        "oanda",
        [UrlProtocol::Rest, UrlProtocol::Stream]
            .map(|p| ("oanda", UrlType::Demo, UrlChannel::Trade, p, "", base)),
    );
    let settings = BTreeMap::from([(
        "oanda".to_string(),
        vike_secrets::venue_setting::VenueSettings::default().with_urls(urls),
    )]);
    let mut keys = vike_secrets::account_credentials::CredentialRegistry::new();
    keys.insert_account(1, "API_KEY", "tok-list");
    keys.insert_account(1, "ACCOUNT_ID", "101-004-1234567-001");
    let row = AccountRow {
        id: 1,
        venue: "oanda".to_string(),
        tier: AccountTier::Demo,
        label: None,
        venue_account_id: None,
        parent_id: None,
        active,
        last_verified_at: None,
        max_exposure: None,
        leverage: None,
        symbol: vike_secrets::fixture_account_symbol("oanda").map(str::to_string),
        facts: Default::default(),
    };
    let accounts = vike_secrets::account_directory::AccountDirectory::from_rows(
        AccountRows::Known(vec![row]),
        None,
    )
    .with_credentials(keys);
    TradeLaneBridge::new(&OANDA_REG, HashMap::new(), &accounts, &settings)
}

/// **A kept session is used only while its account still runs at its tier** (`TradeLaneBridge::still_runs`, read
/// from the rows on each call): a running demo account passes; the same account inactive, or asked for at another
/// tier, is refused in words.
#[test]
fn a_kept_session_is_used_only_while_its_account_runs() {
    let demo = target("oanda", AccountTier::Demo);
    assert_eq!((oanda_lane("http://127.0.0.1:9", true).still_runs(&demo))(), Ok(()));
    let stopped =
        (oanda_lane("http://127.0.0.1:9", false).still_runs(&demo))().expect_err("inactive");
    assert!(stopped.contains("does not run") && stopped.contains("refresh again"), "{stopped}");
    let other =
        (oanda_lane("http://127.0.0.1:9", true).still_runs(&target("oanda", AccountTier::Live)))();
    assert!(other.is_err(), "another tier: {other:?}");
}

/// **An oanda account's start reads its list ONCE, and each refresh reads it anew** (`TradeLaneBridge::hold_oanda_listing`,
/// SYM-LISTS 2026-11-16): one `GET /v3/accounts/{id}/instruments` with the account's own practice keys per read. The
/// start's read is refused (OANDA's 401 around a start): the refusal is recorded in its words, nothing is held and
/// NOTHING asks again by itself (no timer). The refresh (the catalog entry's list) reads through the kept reader and
/// holds the rows. Before, only an order's symbol facts filled the slot, so a running account that had not traded left
/// `oanda.json` unwritten. A `live` account and an account with no keys keep no reader.
#[test]
fn an_oanda_start_reads_its_list_once_and_a_refresh_reads_it_again() {
    use std::io::{Read, Write};
    use vike_catalog::CatalogProvider;

    let demo = target("oanda", AccountTier::Demo);
    assert!(facts_lane(&OANDA_REG).hold_oanda_listing(&demo).is_none(), "no keys: nothing");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let base = format!("http://{}", listener.local_addr().expect("assigned port"));
    let list = r#"{"instruments":[{"name":"EUR_USD","type":"CURRENCY","displayName":"EUR/USD","pipLocation":-4,"displayPrecision":5,"tradeUnitsPrecision":0,"minimumTradeSize":"1","maximumOrderUnits":"100000000","marginRate":"0.03"}]}"#;
    let venue = std::thread::spawn(move || {
        let mut heads = Vec::new();
        for (status, body) in
            [(401, r#"{"errorMessage":"Insufficient authorization"}"#), (200, list)]
        {
            let (mut sock, _) = listener.accept().expect("the list is asked");
            let mut head = Vec::new();
            let mut chunk = [0_u8; 1024];
            while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = sock.read(&mut chunk).expect("read the request");
                if n == 0 {
                    break;
                }
                head.extend_from_slice(&chunk[..n]);
            }
            let reply = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
                 connection: close\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(reply.as_bytes()).expect("answer");
            heads.push(String::from_utf8_lossy(&head).into_owned());
        }
        heads
    });
    let lane = oanda_lane(&base, true);

    let live = target("oanda", AccountTier::Live);
    assert!(lane.hold_oanda_listing(&live).is_none(), "live: no reader");
    lane.hold_oanda_listing(&demo).expect("a demo account with keys reads").join().expect("read");
    let refused =
        vike_catalog::held_list::refused("oanda").expect("the start's refusal is recorded");
    assert!(refused.contains("401"), "{refused}");

    // The operator's refresh: the gatewayhub's catalog entry lists through the kept reader, a NEW read.
    let entry = vike_catalog::held_list::CatalogProviderHeldList { venue: "oanda", classes: &[] };
    assert_eq!(entry.no_session(), None, "a kept reader is a session");
    let rows = entry.list_instruments().expect("the refresh reads it again");
    assert!(rows.iter().any(|r| r.raw_symbol == "EUR_USD" && r.venue == "oanda"), "{rows:?}");
    assert_eq!(vike_catalog::held_list::refused("oanda"), None, "a read that succeeded forgets it");
    assert!(vike_catalog::held_list::held("oanda").is_some_and(|held| held == rows));

    let asked = venue.join().expect("the venue answered");
    assert_eq!(asked.len(), 2, "the start's read and the refresh's, nothing in between");
    for head in &asked {
        assert!(head.starts_with("GET /v3/accounts/101-004-1234567-001/instruments "), "{head}");
    }
}

/// The kept oanda listing of the default account, primed from no network: a USD account listing `EUR_USD` and
/// `EUR_GBP` (docs-shaped rows, parsed by the bridge's own parser, like `symbol_facts_oanda_grid_tests.rs`).
/// The lane's credential-less catalog has no fingerprint, so `of("", None)` is the entry its asks land on.
fn prime_oanda_listing(lane: &TradeLaneBridge) {
    // A USER name is looked up as its kept row's wire name (decision 0290): the bridge's test kept list.
    vike_oanda::test_support::kept_list_for_tests();
    let row = |name: &str| {
        serde_json::json!({
            "name": name, "type": "CURRENCY", "displayName": name.replace('_', "/"),
            "pipLocation": -4, "displayPrecision": 5, "tradeUnitsPrecision": 0,
            "minimumTradeSize": "1", "maximumOrderUnits": "100000000", "marginRate": "0.03",
        })
    };
    let rows = vike_oanda::parse_instruments(
        &serde_json::json!({ "instruments": [row("EUR_USD"), row("EUR_GBP")] }),
    );
    let lister: super::symbol_facts::CurrencyLister =
        std::sync::Arc::new(move || Ok(("USD".to_string(), rows.clone())));
    lane.oanda_facts_for_test()
        .of("", None)
        .facts("EURUSD", lister)
        .expect("a USD pair on a USD account is admitted");
}

/// **oanda converts a quote currency in `symbol_facts_quoted`, and a venue with no rate source is answered by the
/// plain lane** (decision 0183): over one kept listing (no request made), the plain answer REFUSES a
/// GBP-quoted pair for a USD account with the currency fence's words, while the quoted answer goes on to ask
/// for the rate - which this credential-less account cannot have, so it is refused as a failure of the RATE
/// (transient, never par). A pair in the account's own currency is answered identically by both. A venue with
/// no rate source (bybit) reaches the public list (the trap) exactly as through `symbol_facts`. A broken oanda
/// arm turns one of these red: routed to the plain lane the GBP pair is refused for its currency, routed wider
/// the trap is never reached.
#[test]
fn symbol_facts_quoted_routes_oanda_to_the_converting_source_and_bybit_to_the_plain_one() {
    let lane = facts_lane(&OANDA_REG);
    prime_oanda_listing(&lane);
    let oanda = target("oanda", AccountTier::Demo);

    let plain = lane.symbol_facts(&oanda, "EURGBP").expect_err("a GBP quote on a USD account");
    assert_eq!(plain.code, SymbolPropertiesRefusalCode::Verdict, "{plain}");
    assert!(
        plain.reason.contains("quoted in GBP")
            && plain.reason.contains("account's currency is USD"),
        "the plain lane keeps the currency fence: {plain}"
    );
    let quoted = lane.symbol_facts_quoted(&oanda, "EURGBP").expect_err("no credentials, no rate");
    assert_eq!(quoted.code, SymbolPropertiesRefusalCode::RateUnavailable, "{quoted}");
    assert_eq!(
        quoted.reason,
        format!(
            "oanda: {PROPERTIES_RATE_UNAVAILABLE}: no rate for GBP in USD (oanda home conversion \
             unavailable: oanda: this account holds no usable credentials (API_KEY and ACCOUNT_ID))"
        ),
        "the converting lane asked for the rate"
    );

    let own = lane.symbol_facts_quoted(&oanda, "EURUSD").expect("a USD pair needs no rate");
    assert_eq!(
        own.quote_currency, None,
        "quoted in the account's own currency: no currency, no rate"
    );
    // ...and the converting lane STATES the ledger its answer is in (review 2), which the plain one never did
    assert_eq!(own.account_currency.as_deref(), Some("USD"));
    assert_eq!(
        Ok(SymbolPropertiesWire { account_currency: None, ..own }),
        lane.symbol_facts(&oanda, "EURUSD"),
        "both lanes answer it alike"
    );

    let other =
        lane.symbol_facts_quoted(&target("bybit", AccountTier::Demo), "ETHUSDT").expect_err("trap");
    assert!(other.reason.contains("public list must not be asked for"), "{other}");
}

/// **oanda's lane reads an account currency, and a read that fails is said, never guessed**: a `demo` oanda
/// account with no credentials has none and the lane logs why (the ledger then keeps its label); a `live` one is
/// refused the same way; a venue with no currency source (bybit, binance) reads nothing and says nothing. A
/// deleted or widened oanda arm turns this red. (The happy path, the summary's currency, needs a scripted
/// catalog the lane has no seam for; `vike_oanda`'s `catalog_tests` read the summary over HTTP.)
#[test]
fn oanda_reads_an_account_currency_and_a_failed_read_is_said() {
    let lane = facts_lane(&OANDA_REG);
    let read = |venue: &str, tier| {
        let (got, events) =
            vike_log::capture::captured(|| lane.account_currency(&target(venue, tier), &start()));
        let said: Vec<String> = events
            .into_iter()
            .filter(|e| e.level == tracing::Level::WARN)
            .map(|e| e.message)
            .filter(|m| m.contains("the account's currency could not be read"))
            .collect();
        (got, said)
    };
    let (got, said) = read("oanda", AccountTier::Demo);
    assert_eq!(got, None, "no credentials: no currency, never a default");
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].starts_with("oanda:") && said[0].contains("no usable credentials"), "{said:?}");
    let (got, said) = read("oanda", AccountTier::Live);
    assert_eq!(got, None);
    assert!(said.len() == 1 && said[0].contains("practice account only"), "{said:?}");
    for venue in ["bybit", "binance"] {
        let (got, said) = read(venue, AccountTier::Demo);
        assert_eq!((got, said.len()), (None, 0), "{venue} reads no currency here: {said:?}");
    }
}

/// The lane's reply about the account's currency for `venue`'s `account` (a label of its own, so the process-wide
/// "said once" memory of one test never hides another's line), as the wire writes it, with the warn lines it logged.
fn told_for(
    lane: &TradeLaneBridge,
    venue: &str,
    tier: AccountTier,
    account: &str,
    ask: &AdapterStart,
) -> ((Option<String>, bool), Vec<String>) {
    let target =
        TradeLaneTarget { venue: venue.to_string(), account: Some(account.to_string()), tier };
    let (told, events) = vike_log::capture::captured(|| lane.account_currency_stated(&target, ask));
    let warns =
        events.into_iter().filter(|e| e.level == tracing::Level::WARN).map(|e| e.message).collect();
    (account_currency_wire(told.as_ref()), warns)
}

/// **A converting venue whose read of the account's currency came back empty is told to the node as USD,
/// ASSUMED** (owner ruling 2026-10-19, repeated: "the account currency is what the venue API states; if the API
/// does not state it, or the read fails: USD"): the same flag FXCM's lane sets, said once at `warn` in FXCM's words.
/// A demo oanda or ctrader account with no credentials stands for "the read failed" (it can read nothing).
/// Nothing is assumed where the node cannot convert (a `live` or `paper` tier), for a venue with no currency source
/// (bybit), for ig while the node did not ask the budgeted read, or for dukascopy while no sidecar is attached
/// (the ledger is not known YET: its facts say "not now").
#[test]
fn a_converting_demo_account_whose_currency_read_was_empty_is_told_as_assumed_usd() {
    for (registry, venue) in [(&OANDA_REG, "oanda"), (&CTRADER_REG, "ctrader")] {
        let lane = facts_lane(registry);
        let (told, warns) =
            told_for(&lane, venue, AccountTier::Demo, "ASSUMEDUSD", &start_reading_currency());
        assert_eq!(told, (Some("USD".to_string()), true), "{venue}");
        let words = format!("{venue} account currency not reported by the venue: assuming USD");
        assert_eq!(warns.iter().filter(|w| w.contains(&words)).count(), 1, "{venue}: {warns:?}");
        assert!(warns.iter().any(|w| w.contains("no usable credentials")), "the reason: {warns:?}");
        // again: still assumed, and said no second time
        let (again, warns) =
            told_for(&lane, venue, AccountTier::Demo, "ASSUMEDUSD", &start_reading_currency());
        assert_eq!(again, (Some("USD".to_string()), true), "{venue}");
        assert!(warns.iter().all(|w| !w.contains(&words)), "{venue}: said once: {warns:?}");
        // a tier that never converts is told nothing it could act on
        for tier in [AccountTier::Live, AccountTier::Paper] {
            let (told, _) = told_for(&lane, venue, tier, "ASSUMEDTIER", &start());
            assert_eq!(told, (None, false), "{venue} {tier:?}");
        }
    }
    let (told, warns) = told_for(
        &facts_lane(&OANDA_REG),
        "bybit",
        AccountTier::Demo,
        "ASSUMEDBYBIT",
        &start_reading_currency(),
    );
    assert_eq!((told, warns.len()), ((None, false), 0), "no currency source: {warns:?}");
    // ig: a login and a read out of IG's allowance are spent only for a start that asks (`read_account_currency`)
    let ig = facts_lane(&IG_REG);
    let (told, warns) = told_for(&ig, "ig", AccountTier::Demo, "ASSUMEDIG", &start());
    assert_eq!((told, warns.len()), ((None, false), 0), "not asked: {warns:?}");
    let (told, _) = told_for(&ig, "ig", AccountTier::Demo, "ASSUMEDIG", &start_reading_currency());
    assert_eq!(told, (Some("USD".to_string()), true), "asked, and the read came back empty");
    // dukascopy: no sidecar attached is "not now", never USD
    let (told, warns) = told_for(
        &facts_lane(&DUKASCOPY_REG),
        "dukascopy",
        AccountTier::Demo,
        "ASSUMEDDUK",
        &start(),
    );
    assert_eq!((told, warns.len()), ((None, false), 0), "no sidecar attached: {warns:?}");
}

/// A reconcile client that reports `cash` and counts how often the venue's balance was asked.
struct Cash {
    cash: f64,
    asked: AtomicUsize,
}

impl vike_execution::recon::ReconcileClient for Cash {
    fn fetch_order_status_reports(
        &self,
        _since: i64,
    ) -> Result<Vec<vike_model::OrderStatusReport>, String> {
        Ok(Vec::new())
    }
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<vike_model::FillReport>, String> {
        Ok(Vec::new())
    }
    fn fetch_position_status_reports(
        &self,
    ) -> Result<Vec<vike_model::PositionStatusReport>, String> {
        Ok(Vec::new())
    }
    fn fetch_balance(&self) -> Result<Option<f64>, vike_execution::recon::ReconcileFetchError> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        Ok(Some(self.cash))
    }
}

/// **The balance of an account whose currency is only ASSUMED is not handed to the node** (review MEDIUM-2: the
/// figure of oanda, ig and fxcm is the account's home-currency cash with no currency beside it, and the node
/// adopts it into a ledger labelled with the assumed USD): the pass is answered `None` ("not reported") without
/// asking the venue, from the first silent read until the venue states the currency, and only for these venues
/// (ctrader fences itself, ibkr's client withholds itself, dukascopy has no leg). Every other fetch is untouched.
#[test]
fn the_balance_of_an_account_whose_currency_is_only_assumed_is_not_handed_to_the_node() {
    use super::assumed_currency::{CURRENCY_SAID, source_currency, start_currency};
    use crate::trade::recon::fetch_for;
    use vike_gatewayhub_client::proto::{ReconcileKind, ReconcilePayload};
    use vike_model::AccountCurrency;

    let lane = facts_lane(&OANDA_REG);
    let target = |venue: &str, account: &str| TradeLaneTarget {
        venue: venue.to_string(),
        account: Some(account.to_string()),
        tier: AccountTier::Demo,
    };
    for venue in ["oanda", "ig"] {
        // an account the venue STATED: its balance lands, and a later silent read does not take that away
        let client = Cash { cash: 12.5, asked: AtomicUsize::new(0) };
        let stated = target(venue, "BALSTATED");
        start_currency(&CURRENCY_SAID, venue, "BALSTATED", AccountCurrency::Stated("USD".into()));
        assert_eq!(lane.balance_withheld(&stated), None, "{venue}: stated");
        assert_eq!(
            fetch_for(&client, ReconcileKind::Balance, &lane, &stated),
            Ok(ReconcilePayload::Balance(Some(12.5))),
            "{venue}: a stated currency's balance lands"
        );
        start_currency(
            &CURRENCY_SAID,
            venue,
            "BALSTATED",
            AccountCurrency::Assumed { why: "no row".into() },
        );
        source_currency(&CURRENCY_SAID, venue, "BALSTATED", Err("timed out".into()));
        assert_eq!(lane.balance_withheld(&stated), None, "{venue}: never downgraded to assumed");

        // an account whose START read nothing: assumed, and its balance is not handed to the node
        let t = target(venue, "BALASSUMED");
        start_currency(
            &CURRENCY_SAID,
            venue,
            "BALASSUMED",
            AccountCurrency::Assumed { why: "no row".into() },
        );
        let why = lane.balance_withheld(&t).expect("assumed: withheld");
        assert!(why.contains("only ASSUMED") && why.contains(venue), "{venue}: {why}");
        assert_eq!(
            fetch_for(&client, ReconcileKind::Balance, &lane, &t),
            Ok(ReconcilePayload::Balance(None)),
            "{venue}: not reported"
        );
        assert_eq!(
            client.asked.load(Ordering::SeqCst),
            1,
            "{venue}: the venue was not asked again"
        );
        assert!(
            matches!(
                fetch_for(&client, ReconcileKind::Positions, &lane, &t),
                Ok(ReconcilePayload::Positions(p)) if p.is_empty()
            ),
            "{venue}: every other fetch is untouched"
        );
        // a RATE SOURCE that now reads the currency (EUR) does not lift it: the engine is still the start's USD
        source_currency(&CURRENCY_SAID, venue, "BALASSUMED", Ok("EUR".into()));
        assert!(lane.balance_withheld(&t).is_some(), "{venue}: only a start moves it");
        // ...the next start, which reads the venue, does
        start_currency(&CURRENCY_SAID, venue, "BALASSUMED", AccountCurrency::Stated("EUR".into()));
        assert_eq!(lane.balance_withheld(&t), None, "{venue}: stated by a start");
    }
    // the venues that fence themselves are never withheld by the lane, whatever was said about them
    for venue in ["ctrader", "ibkr", "dukascopy", "bybit"] {
        start_currency(
            &CURRENCY_SAID,
            venue,
            "BALASSUMED",
            AccountCurrency::Assumed { why: "no row".into() },
        );
        assert_eq!(lane.balance_withheld(&target(venue, "BALASSUMED")), None, "{venue}");
    }
}

/// A FXCM session that states `BASE_CRNCY` `code`, behind the real registry (`vike_fxcm::account::reads`).
struct StatesCurrency(&'static str);

impl vike_fxcm::account::reads::SessionReads for StatesCurrency {
    fn system_properties(&self) -> vike_fxcm::account::quote_rate::PropertiesRead {
        vike_fxcm::account::quote_rate::PropertiesRead::Json(
            serde_json::json!({ "BASE_CRNCY": self.0 }).to_string(),
        )
    }
    fn offers(&self) -> Result<String, String> {
        Err("no offers in this fixture".to_string())
    }
}

/// **FXCM's balance gate reads what the START told the node, never what the session says now** (review 2, HIGH-1):
/// the account was started with no session behind it (the ruling's USD, ASSUMED), and a session that states EUR
/// registers afterwards, so `currency_is_assumed` (the session's own decision) says "stated" while the engine is
/// still the start's assumed USD, and the EUR cash would be adopted as dollars. The balance stays withheld until a
/// START reads the currency, and that start is told EUR, unflagged, whatever the engine is mounted on (HIGH-2).
#[test]
fn fxcms_balance_gate_reads_what_the_start_told_not_what_the_session_says_now() {
    let lane = facts_lane(&OANDA_REG);
    let t = TradeLaneTarget {
        venue: "fxcm".to_string(),
        account: Some("FXTOLD".to_string()),
        tier: AccountTier::Demo,
    };
    let ask = AdapterStart { symbol: "EURUSD".to_string(), ..start() };
    assert_eq!(
        account_currency_wire(lane.account_currency_stated(&t, &ask).as_ref()),
        (Some("USD".to_string()), true)
    );
    assert!(lane.balance_withheld(&t).is_some(), "assumed at the start: withheld");

    let entry =
        vike_fxcm::account::reads::register("FXTOLD", std::sync::Arc::new(StatesCurrency("EUR")));
    assert!(
        lane.balance_withheld(&t).is_some(),
        "the session states EUR now, but the engine the start anchored is still the assumed USD"
    );

    assert_eq!(
        account_currency_wire(lane.account_currency_stated(&t, &ask).as_ref()),
        (Some("EUR".to_string()), false),
        "a start that reads the venue is told what it states, whatever the mount is quoted in"
    );
    assert_eq!(lane.balance_withheld(&t), None, "the next engine is EUR, and its balance lands");
    vike_fxcm::account::reads::unregister("FXTOLD", &entry);
}

/// **A rate stamp carries the PROOF that the venue named the currency it states** (A52 (b),
/// `SymbolPropertiesWire::account_currency_stated`): the stamp reads `USD` for an assumed USD exactly as for a stated one, so
/// the proof is the memory of what the venue itself said (oanda, ctrader), true for THAT currency only. An answer
/// with no ledger stamp, an assumption, a failed read and a venue whose rates state no ledger prove nothing.
#[test]
fn a_rate_stamp_carries_the_proof_that_the_venue_named_the_currency() {
    use super::assumed_currency::{CURRENCY_SAID, source_currency, start_currency};
    use vike_model::AccountCurrency;
    let lane = facts_lane(&OANDA_REG);
    let on = |venue: &str, account: &str| TradeLaneTarget {
        venue: venue.to_string(),
        account: Some(account.to_string()),
        tier: AccountTier::Demo,
    };
    let stamped = |currency: &str| SymbolPropertiesWire {
        account_currency: Some(currency.to_string()),
        ..SymbolPropertiesWire::default()
    };
    for venue in ["oanda", "ctrader"] {
        let t = on(venue, "STAMPPROOF");
        let proven =
            |currency: &str| lane.stamp_stated(&t, stamped(currency)).account_currency_stated;
        assert!(!proven("USD"), "{venue}: nothing said yet");
        start_currency(
            &CURRENCY_SAID,
            venue,
            "STAMPPROOF",
            AccountCurrency::Assumed { why: "no row".into() },
        );
        source_currency(&CURRENCY_SAID, venue, "STAMPPROOF", Err("timed out".into()));
        assert!(!proven("USD"), "{venue}: an assumed USD stamps USD and proves nothing");
        source_currency(&CURRENCY_SAID, venue, "STAMPPROOF", Ok("USD".into()));
        assert!(proven("USD"), "{venue}: the venue named USD");
        assert!(!proven("EUR"), "{venue}: ...and did not name EUR");
        assert!(
            !lane.stamp_stated(&t, SymbolPropertiesWire::default()).account_currency_stated,
            "{venue}: an answer that states no ledger proves nothing"
        );
    }
    // a venue whose rates state no ledger of their own is never proven, whatever the memory holds
    for venue in ["ibkr", "ig", "dukascopy", "bybit"] {
        source_currency(&CURRENCY_SAID, venue, "STAMPPROOF", Ok("USD".into()));
        assert!(
            !lane.stamp_stated(&on(venue, "STAMPPROOF"), stamped("USD")).account_currency_stated,
            "{venue}"
        );
    }
}

/// **The belt lets go together with the node's lift, for the assumed currency only** (A52 (b)): an oanda or ig
/// account whose start assumed USD is withheld until a rate source reads the venue's word; EUR keeps it withheld
/// (EUR cash under a USD label), USD ends it (the label was right).
#[test]
fn the_belt_lets_go_when_the_venue_states_the_very_usd_the_start_assumed() {
    use super::assumed_currency::{CURRENCY_SAID, source_currency, start_currency};
    use vike_model::AccountCurrency;
    let lane = facts_lane(&OANDA_REG);
    for venue in ["oanda", "ig"] {
        let t = TradeLaneTarget {
            venue: venue.to_string(),
            account: Some("BELTLIFT".to_string()),
            tier: AccountTier::Demo,
        };
        start_currency(
            &CURRENCY_SAID,
            venue,
            "BELTLIFT",
            AccountCurrency::Assumed { why: "no row".into() },
        );
        assert!(lane.balance_withheld(&t).is_some(), "{venue}: assumed at the start");
        source_currency(&CURRENCY_SAID, venue, "BELTLIFT", Ok("EUR".into()));
        assert!(lane.balance_withheld(&t).is_some(), "{venue}: EUR is not what the label says");
        source_currency(&CURRENCY_SAID, venue, "BELTLIFT", Ok("USD".into()));
        assert_eq!(lane.balance_withheld(&t), None, "{venue}: the venue confirmed the label");
    }
}

/// **FXCM's stamp is proven by its session's own word, and its belt follows a session that now STATES the USD the
/// start assumed** (A52 (b)): the account was started with no session (the ruling's USD, ASSUMED, withheld), and a
/// session that states USD registers afterwards: the stamp is proven for USD only, and the belt lets go with
/// the node's lift. A session that assumed its own USD proves nothing and keeps the belt.
#[test]
fn fxcms_stamp_is_proven_by_the_sessions_own_word_and_its_belt_follows() {
    let lane = facts_lane(&OANDA_REG);
    let t = TradeLaneTarget {
        venue: "fxcm".to_string(),
        account: Some("FXPROOF".to_string()),
        tier: AccountTier::Demo,
    };
    let stamped = |currency: &str| SymbolPropertiesWire {
        account_currency: Some(currency.to_string()),
        ..SymbolPropertiesWire::default()
    };
    assert!(
        !lane.stamp_stated(&t, stamped("USD")).account_currency_stated,
        "no session: nothing was stated"
    );
    let ask = AdapterStart { symbol: "EURUSD".to_string(), ..start() };
    assert_eq!(
        account_currency_wire(lane.account_currency_stated(&t, &ask).as_ref()),
        (Some("USD".to_string()), true)
    );
    assert!(lane.balance_withheld(&t).is_some(), "assumed at the start: withheld");

    let entry =
        vike_fxcm::account::reads::register("FXPROOF", std::sync::Arc::new(StatesCurrency("USD")));
    assert!(lane.stamp_stated(&t, stamped("USD")).account_currency_stated, "the session named USD");
    assert!(!lane.stamp_stated(&t, stamped("EUR")).account_currency_stated, "...and not EUR");
    assert_eq!(lane.balance_withheld(&t), None, "the session confirms the label the start assumed");
    vike_fxcm::account::reads::unregister("FXPROOF", &entry);
}

static IG: PlantedAdapter =
    PlantedAdapter::new("ig", ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials));
static IG_REG: [VenueAdapterRow; 1] = [VenueAdapterRow::Adapter(&IG)];

/// **ig's facts are read with the account's own login, for a `demo` account only**: a `live` or `paper`
/// account is refused (the adapter reaches the demo gateway alone), an account with no credentials is
/// refused in words before any login is attempted (nothing is dialled, and the public list, a trap here,
/// is never asked), and a build with no ig row answers the default.
#[test]
fn ig_symbol_facts_come_from_the_accounts_own_login() {
    const EPIC: &str = "CS.D.EURUSD.MINI.IP";
    let lane = facts_lane(&IG_REG);
    for tier in [AccountTier::Live, AccountTier::Paper] {
        let why = lane.symbol_facts(&target("ig", tier), EPIC).expect_err("not demo");
        assert!(why.reason.contains("demo gateway only"), "{tier:?}: {why}");
    }
    let none = lane.symbol_facts(&target("ig", AccountTier::Demo), EPIC).expect_err("no key");
    assert_eq!(
        none,
        super::symbol_facts::verdict(IG_HOLDS_NO_CREDENTIALS),
        "no credentials: refused before a login, asked of nothing"
    );
    let absent = facts_lane(&SPEAKS_REG)
        .symbol_facts(&target("ig", AccountTier::Demo), EPIC)
        .expect_err("no ig row");
    assert_eq!(absent, super::symbol_facts::verdict(super::symbol_facts::NOT_ANSWERED));
}

/// **The converting ask of ig has the same gates as the plain one** (decision 0183): a `live` or `paper`
/// account is refused (the adapter reaches the demo gateway alone), an account with no credentials is refused
/// in words before any login, and a build with no ig row answers the default. The currency the venue states
/// for the account is read through the same gates: with none of them met it is `None`, never a guess.
#[test]
fn ig_quoted_facts_and_the_accounts_currency_have_the_same_gates_as_the_plain_ask() {
    const EPIC: &str = "CS.D.USDJPY.MINI.IP";
    let lane = facts_lane(&IG_REG);
    for tier in [AccountTier::Live, AccountTier::Paper] {
        let why = lane.symbol_facts_quoted(&target("ig", tier), EPIC).expect_err("not demo");
        assert!(why.reason.contains("demo gateway only"), "{tier:?}: {why}");
        assert_eq!(lane.account_currency(&target("ig", tier), &start_reading_currency()), None);
    }
    let demo = target("ig", AccountTier::Demo);
    let none = lane.symbol_facts_quoted(&demo, EPIC).expect_err("no key");
    assert_eq!(
        none,
        super::symbol_facts::verdict(IG_HOLDS_NO_CREDENTIALS),
        "no credentials: refused before a login, asked of nothing"
    );
    assert_eq!(
        lane.account_currency(&demo, &start_reading_currency()),
        None,
        "no credentials: no currency, never a default"
    );
    let absent = facts_lane(&SPEAKS_REG).symbol_facts_quoted(&demo, EPIC).expect_err("no ig row");
    assert_eq!(absent, super::symbol_facts::verdict(super::symbol_facts::NOT_ANSWERED));
    // a venue with no rate source is answered exactly what its plain ask says
    let ctrader = target("ctrader", AccountTier::Demo);
    assert_eq!(
        facts_lane(&SPEAKS_REG).symbol_facts_quoted(&ctrader, "EURGBP"),
        facts_lane(&SPEAKS_REG).symbol_facts(&ctrader, "EURGBP")
    );
}

/// **An IG account in another currency than USD is told to the node AS WHAT IT IS**, like every venue's: the node
/// converts for it (its ledger is the account's money, the mounted EUR/USD epic is declared quoted in USD and rated
/// by this lane's facts), where it used to refuse to run it. Telling nothing would leave the node unable to tell
/// "not read" from "not USD". A dollar account is told; oanda's own statement is passed through whatever it is; a
/// failed read tells nothing.
#[test]
fn an_ig_account_in_another_currency_is_told_as_it_is() {
    use super::bridge_trade::told_currency;
    let read = |text: &str| Ok(text.to_string());
    assert_eq!(told_currency("ig", "a", read("USD")), Some("USD".to_string()));
    assert_eq!(told_currency("ig", "a", read("GBP")), Some("GBP".to_string()), "told, not hidden");
    assert_eq!(told_currency("ig", "a", read("JPY")), Some("JPY".to_string()));
    assert_eq!(told_currency("oanda", "a", read("GBP")), Some("GBP".to_string()));
    assert_eq!(told_currency("ig", "a", Err("ig login: timeout".to_string())), None);
    assert_eq!(told_currency("oanda", "a", Err("no".to_string())), None);
}

/// **A currency read is bounded**: oanda's `GET /summary` is asked again on a same-start re-ask (a reconnect after
/// a failed first read, or onto an adapter a restore installed) on the lease reader thread under the session's
/// adapter lock, and used to wait for the shared agent's 30 s. A read that answers in time is its answer; one that
/// does not is an error naming the wait, in about the wait, whatever the read goes on to do.
#[test]
fn a_currency_read_is_waited_on_for_its_bound_and_no_longer() {
    use super::bridge_trade::read_within;
    use std::time::{Duration, Instant};
    let quick =
        read_within(Duration::from_secs(5), "account-currency read", || Ok("GBP".to_string()));
    assert_eq!(quick, Ok("GBP".to_string()));
    let failed = read_within(Duration::from_secs(5), "account-currency read", || {
        Err::<String, _>("oanda: 401".to_string())
    });
    assert_eq!(failed, Err("oanda: 401".to_string()), "the venue's own failure passes through");
    let began = Instant::now();
    let slow = read_within(Duration::from_millis(100), "account-currency read", || {
        std::thread::sleep(Duration::from_secs(3));
        Ok("GBP".to_string())
    });
    let why = slow.expect_err("too slow");
    assert!(
        why.contains("account-currency read") && why.contains("did not answer within"),
        "{why}"
    );
    assert!(
        began.elapsed() < Duration::from_secs(2),
        "the wait is the bound: {:?}",
        began.elapsed()
    );
}

#[cfg(feature = "ibkr")]
static IBKR: PlantedAdapter =
    PlantedAdapter::new("ibkr", ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials));
#[cfg(feature = "ibkr")]
static IBKR_REG: [VenueAdapterRow; 1] = [VenueAdapterRow::Adapter(&IBKR)];

/// **ibkr's facts come from the account's own gateway, never the public list**: a tier other than `demo` is
/// refused by name, an account with no `ACCOUNT` field looks nothing up (nothing is dialled), a build with no
/// ibkr row answers the default. A name the grammar refuses is refused before any dial.
#[cfg(feature = "ibkr")]
#[test]
fn ibkr_symbol_facts_come_from_the_accounts_own_gateway() {
    let lane = facts_lane(&IBKR_REG);
    let live = lane.symbol_facts(&target("ibkr", AccountTier::Live), "AAPL");
    assert!(live.expect_err("live").reason.contains("`demo` account only"));
    let none = lane
        .symbol_facts(&target("ibkr", AccountTier::Demo), "AAPL")
        .expect_err("no account field");
    assert_eq!(
        none,
        super::symbol_facts::verdict(
            "ibkr: this account holds no usable ACCOUNT field (or its gateway rows or addresses do \
             not load), so no stock is looked up for it"
        ),
        "no account field: refused before a dial, asked of nothing"
    );
    let absent = facts_lane(&SPEAKS_REG)
        .symbol_facts(&target("ibkr", AccountTier::Demo), "AAPL")
        .expect_err("no ibkr row");
    assert_eq!(absent, super::symbol_facts::verdict(super::symbol_facts::NOT_ANSWERED));
}

/// **An account-credentialed lookup resolves its account the one way, in one order** (`with_lookup_inputs`,
/// five copies until it was one): after the venue's tier fence, a build with no row answers the default even
/// for an account name that does not parse, and a row with an account name that does not parse is refused
/// naming the venue and the block, before any credential is read.
#[test]
fn every_credentialed_lookup_refuses_an_unparseable_account_naming_its_venue() {
    #[cfg_attr(
        not(feature = "ibkr"),
        expect(unused_mut, reason = "the `ibkr` feature adds its row")
    )]
    let mut rows: Vec<(&str, &'static [VenueAdapterRow], &str)> = vec![
        ("alpaca", &ALPACA_REG[..], "AAPL"),
        ("ctrader", &CTRADER_REG[..], "EURUSD"),
        ("oanda", &OANDA_REG[..], "GBPUSD"),
        // ig's USER name of its EUR/USD Mini epic (decision 0290; the owner's ig names of 2026-11-17).
        ("ig", &IG_REG[..], "EURUSD"),
    ];
    #[cfg(feature = "ibkr")]
    rows.push(("ibkr", &IBKR_REG[..], "AAPL"));
    for (venue, registry, symbol) in rows {
        let bad = TradeLaneTarget {
            venue: venue.to_string(),
            account: Some("not a label".to_string()),
            tier: AccountTier::Demo,
        };
        let why = facts_lane(registry).symbol_facts(&bad, symbol).expect_err("no such account");
        assert_eq!(
            why,
            super::symbol_facts::verdict(format!(
                "{venue}: no account to look up for: {}",
                ArmingBlock::NoAccountRow.as_str()
            )),
            "{venue}"
        );
        let absent = facts_lane(&SPEAKS_REG).symbol_facts(&bad, symbol).expect_err("no row");
        assert_eq!(
            absent,
            super::symbol_facts::verdict(super::symbol_facts::NOT_ANSWERED),
            "{venue}: the row is asked first"
        );
    }
}

static DUKASCOPY: PlantedAdapter = PlantedAdapter::new(
    "dukascopy",
    ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials),
);
static DUKASCOPY_REG: [VenueAdapterRow; 1] = [VenueAdapterRow::Adapter(&DUKASCOPY)];

/// The account's instrument list a dukascopy sidecar states at its login, put on the process's bus (the one
/// `TradeLaneBridge::dukascopy_symbol_facts` reads): the bridge's recorded-shape fixture; and the kept list the datahub
/// keeps of it, which turns the node's user name into the row's wire name (decision 0290).
fn dukascopy_list_stated() {
    vike_dukascopy::feed::process_bus().set_instrument_list(vike_dukascopy::test_support::listed());
    vike_dukascopy::test_support::hold_kept_list_for_tests();
}

/// **dukascopy's facts are the account's list's, as the sidecar stated it, never the public list and never a dial**:
/// the grid of a USD-quoted pair comes back whatever the (trap) public list would say, another quote currency, a CFD
/// and a name the list lacks are each refused by name, a tier other than `demo` is refused, and a build with no
/// dukascopy row answers the default.
#[test]
fn dukascopy_symbol_facts_come_from_the_accounts_list() {
    dukascopy_list_stated();
    let lane = facts_lane(&DUKASCOPY_REG);
    let demo = |symbol: &str| lane.symbol_facts(&target("dukascopy", AccountTier::Demo), symbol);
    let facts = demo("EURUSD").expect("a USD-quoted pair");
    assert_eq!(facts.properties.tick_size, 0.00001);
    assert_eq!((facts.properties.step_size, facts.properties.min_qty), (1000.0, 1000.0));
    assert_eq!(facts.contract_value, None, "linear: a unit of quantity is a unit of the base");
    assert!(demo("USDJPY").expect_err("quote").reason.contains("quoted in JPY"));
    // a metal or a CFD needs the venue's own terms, which only the converting lane reads from the sidecar
    for symbol in ["AAPL.CFD", "XAUUSD"] {
        assert!(demo(symbol).expect_err("terms").reason.contains("the venue's own"), "{symbol}");
    }
    // a name no kept row answers (the wire name, the SDK's enum-style one, a pair the list lacks): a verdict in words
    for symbol in ["AAPL.US/USD", "AAPLUSUSD", "GBPNOK"] {
        let why = demo(symbol).expect_err("no row");
        assert_eq!(why.code, SymbolPropertiesRefusalCode::Verdict, "{symbol}: {why}");
        assert!(why.reason.contains("carries no row"), "{symbol}: {why}");
    }
    for tier in [AccountTier::Live, AccountTier::Paper] {
        let why = lane.symbol_facts(&target("dukascopy", tier), "EURUSD").expect_err("tier");
        assert!(why.reason.contains("`demo` account only"), "{tier:?}: {why}");
    }
    let absent = facts_lane(&SPEAKS_REG)
        .symbol_facts(&target("dukascopy", AccountTier::Demo), "EURUSD")
        .expect_err("no dukascopy row");
    assert_eq!(absent, super::symbol_facts::verdict(super::symbol_facts::NOT_ANSWERED));
}

/// **A node that converts is answered by the same lane, and every state that cannot convert is the unconverting
/// answer, word for word** (decision 0183): a `live` or `paper` tier is the tier refusal, a build with no
/// dukascopy row the default, and a process whose price bus has NO sidecar attached (no jar, no account armed, or
/// one that is restarting) states no account currency, so a USD pair is admitted as before, an instrument the table
/// refuses by name keeps its words, a pair quoted in another currency is "not now" (the ledger is not known YET:
/// never remembered as a verdict), and the lane's `account_currency` is `None` (the engine keeps its label). The
/// converting answers themselves, over a scripted sidecar, are the bridge's (`crates/bridges/dukascopy/src/ledger_tests.rs`).
#[test]
fn dukascopy_symbol_facts_quoted_falls_back_to_the_unconverting_answer_without_a_stated_currency() {
    dukascopy_list_stated();
    let lane = facts_lane(&DUKASCOPY_REG);
    let demo = target("dukascopy", AccountTier::Demo);
    for symbol in ["EURUSD", "AAPLUSUSD", "GBPNOK"] {
        assert_eq!(
            lane.symbol_facts_quoted(&demo, symbol),
            lane.symbol_facts(&demo, symbol),
            "{symbol}"
        );
    }
    // a metal or a CFD reads the venue's terms off the sidecar: with none attached it is "not now", never a verdict
    for symbol in ["XAUUSD", "AAPL.CFD"] {
        let why = lane.symbol_facts_quoted(&demo, symbol).expect_err(symbol);
        assert!(why.reason.contains("sidecar is not up"), "{symbol}: {why}");
        assert_eq!(why.code, SymbolPropertiesRefusalCode::RateUnavailable, "{why}");
    }
    for symbol in ["USDJPY", "EURGBP"] {
        let why = lane.symbol_facts_quoted(&demo, symbol).expect_err(symbol);
        assert!(why.reason.contains("sidecar is not up"), "{symbol}: {why}");
        assert_eq!(why.code, SymbolPropertiesRefusalCode::RateUnavailable, "{why}");
        // the unconverting lane keeps its own fence for a node that does not convert
        let fence = lane.symbol_facts(&demo, symbol).expect_err("fence");
        assert_eq!(fence.code, SymbolPropertiesRefusalCode::Verdict, "{fence}");
        assert!(fence.reason.contains("quoted in"), "{fence}");
    }
    assert!(lane.symbol_facts_quoted(&demo, "GBPUSD").is_ok());
    for tier in [AccountTier::Live, AccountTier::Paper] {
        let why = lane.symbol_facts_quoted(&target("dukascopy", tier), "USDJPY").expect_err("tier");
        assert!(why.reason.contains("`demo` account only"), "{tier:?}: {why}");
        assert_eq!(lane.account_currency(&target("dukascopy", tier), &start()), None, "{tier:?}");
    }
    assert_eq!(
        lane.account_currency(&demo, &start()),
        None,
        "no sidecar attached: no currency, never USD"
    );
    let absent = facts_lane(&SPEAKS_REG)
        .symbol_facts_quoted(&target("dukascopy", AccountTier::Demo), "EURUSD")
        .expect_err("no dukascopy row");
    assert_eq!(absent, super::symbol_facts::verdict(super::symbol_facts::NOT_ANSWERED));
}

/// **A dukascopy start that is REFUSED mounts nothing**: the planted adapter holds no credentials, so its start is
/// declined, and the lane's mount stays the symbol the account's row STATES (`EURUSD`; owner, 2026-11-13: no
/// compiled symbol) whatever symbol the declined start named. An account whose row states none has no mount.
#[test]
fn a_refused_dukascopy_start_leaves_the_mount_the_accounts_own() {
    let demo = target("dukascopy", AccountTier::Demo);
    let policy =
        vike_tradehub_core::AdapterPolicy::default().with_account_row(vike_secrets::AccountRow {
            symbol: Some("EURUSD".to_string()),
            ..vike_secrets::account_row(1, "dukascopy", AccountTier::Demo, None, true)
        });
    let lane = lane(&DUKASCOPY_REG, &policy);
    assert_eq!(lane.dukascopy_mount(&demo), "EURUSD", "before any start");
    let on = |symbol: &str| AdapterStart { symbol: symbol.to_string(), ..start() };
    let refused =
        lane.start_adapter(&demo, &on("USDJPY"), events(), None).expect_err("no credentials");
    assert!(!refused.refusal_text.is_empty());
    assert_eq!(lane.dukascopy_mount(&demo), "EURUSD", "a declined start remembers nothing");
    assert_eq!(
        facts_lane(&DUKASCOPY_REG).dukascopy_mount(&demo),
        "",
        "no row states a symbol: no mount, never a compiled one"
    );
}

/// A dukascopy row that comes up LIVE and declares the one-sidecar resource under a name of its own (the claim set
/// is process-global), so the lane's remembered mount can be read after a start that came up and after starts that
/// were declined, the way the real bridge declines a second account.
struct Mounts;

impl VenueAdapter for Mounts {
    fn venue(&self) -> &'static str {
        "dukascopy"
    }
    fn declaration(&self) -> VenueAdapterDeclaration {
        super::testutil::exclusive_declaration("the dukascopy-mount test's resource")
    }
    fn resolve(&self, _inputs: &AdapterInputs<'_>) -> ArmingResolution {
        ArmingResolution::Armed { tier: AccountTier::Demo, held_below_live: None }
    }
    fn start(&self, _req: AdapterRequest<'_>) -> AdapterOutcome {
        AdapterOutcome {
            exec: VenueAdapterExecutionOutcome::Running(VenueAdapterExecutionRunning {
                client: Box::new(vike_tradehub_core::test_support::doubles::NoopClient),
                bound_tier: AccountTier::Demo,
                grid: None,
                contract_value: None,
                margin_mode: None,
                leg_grids: Vec::new(),
            }),
            reconcile: None,
            identity: None,
        }
    }
}

static MOUNTS: Mounts = Mounts;
static MOUNTS_REG: [VenueAdapterRow; 1] = [VenueAdapterRow::Adapter(&MOUNTS)];

/// **The money the dukascopy ledger books at par is the symbol of the engine that holds the sidecar**, not a
/// DECLINED start's: the asking account's own row's (`EURUSD` for the default account, `EURGBP` for the labelled one)
/// until a start came up, then the last start that came up, whatever order the accounts were started in
/// (the default account first, then the labelled holder, and the other way round), unmoved by another venue's
/// start and kept by the next generation of the lane. A declined second start (the default account that yields,
/// the holder asked again while it runs) must not change how the RUNNING holder's facts are judged.
#[test]
fn the_dukascopy_mount_is_the_symbol_of_the_start_that_came_up_and_no_declined_start_moves_it() {
    let policy = vike_tradehub_core::AdapterPolicy::default()
        .with_account_row(vike_secrets::AccountRow {
            symbol: Some("EURUSD".to_string()),
            ..vike_secrets::account_row(1, "dukascopy", AccountTier::Demo, None, true)
        })
        .with_account_row(vike_secrets::AccountRow {
            symbol: Some("EURGBP".to_string()),
            ..vike_secrets::account_row(2, "dukascopy", AccountTier::Demo, Some("ALT"), true)
        });
    let lane = lane(&MOUNTS_REG, &policy);
    let on = |symbol: &str| AdapterStart { symbol: symbol.to_string(), ..start() };
    let default_account = target("dukascopy", AccountTier::Demo);
    let labelled = TradeLaneTarget {
        venue: "dukascopy".to_string(),
        account: Some("ALT".to_string()),
        tier: AccountTier::Demo,
    };
    assert_eq!(lane.dukascopy_mount(&default_account), "EURUSD", "before any start");
    assert_eq!(
        lane.dukascopy_mount(&labelled),
        "EURGBP",
        "each account's own row, before any start"
    );

    // the boot order: the default account first (it yields to the labelled row), the labelled holder last
    let yielded = lane
        .start_adapter(&default_account, &on("USDJPY"), events(), None)
        .expect_err("the default account yields the sidecar to the active labelled row");
    assert!(!yielded.refusal_text.is_empty());
    assert_eq!(
        lane.dukascopy_mount(&default_account),
        "EURUSD",
        "the account that yielded mounted nothing"
    );
    let holder = lane
        .start_adapter(&labelled, &on("EURGBP"), events(), None)
        .unwrap_or_else(|refused| panic!("the holder starts: {}", refused.refusal_text));
    assert_eq!(lane.dukascopy_mount(&default_account), "EURGBP", "the holder's mount");

    // while the holder runs, every other start is declined and moves nothing
    assert!(lane.start_adapter(&default_account, &on("GBPJPY"), events(), None).is_err());
    let again = lane
        .start_adapter(&labelled, &on("USDJPY"), events(), None)
        .expect_err("the process's one sidecar is claimed: a second start is refused");
    assert!(!again.refusal_text.is_empty());
    assert_eq!(
        lane.dukascopy_mount(&default_account),
        "EURGBP",
        "a declined second start leaves the holder's mount"
    );
    let _ =
        lane.start_adapter(&target("nowhere", AccountTier::Demo), &on("BTCUSDT"), events(), None);
    assert_eq!(
        lane.dukascopy_mount(&default_account),
        "EURGBP",
        "another venue's start moves nothing"
    );
    let next = lane.successor(HashMap::new(), super::testutil::snapshot_of(&policy));
    assert_eq!(
        next.dukascopy_mount(&default_account),
        "EURGBP",
        "a refreshed generation of the lane keeps it"
    );

    // the holder ends and its account starts again on another pair: that start came up, so it is the mount
    drop(holder);
    let _restarted = lane
        .start_adapter(&labelled, &on("AUDUSD"), events(), None)
        .unwrap_or_else(|refused| panic!("the restart runs: {}", refused.refusal_text));
    assert_eq!(
        lane.dukascopy_mount(&default_account),
        "AUDUSD",
        "a start that came up moves the mount"
    );
}

// ---- an operator's read ----------------------------------------------------------------------------

/// A venue whose bridge answers an operator's read, recording the tier, the account and the kind it was
/// handed (so the test can see the lane built the inputs of THIS account, not another's).
struct Reads {
    asked: std::sync::Mutex<Vec<(AccountTier, Option<String>, vike_model::VenueReadKind)>>,
}

impl VenueAdapter for Reads {
    fn venue(&self) -> &'static str {
        "bybit"
    }
    fn declaration(&self) -> VenueAdapterDeclaration {
        PLANTED_DECLARATION
    }
    fn resolve(&self, _inputs: &AdapterInputs<'_>) -> ArmingResolution {
        ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials)
    }
    fn start(&self, _req: AdapterRequest<'_>) -> AdapterOutcome {
        AdapterOutcome::paper()
    }
    fn venue_read(
        &self,
        inputs: &AdapterInputs<'_>,
        kind: vike_model::VenueReadKind,
    ) -> Result<vike_model::VenueRead, vike_utility::errors::CarriedRefusal> {
        self.asked.lock().expect("asked").push((
            inputs.tier,
            inputs.account.text().map(str::to_string),
            kind,
        ));
        Ok(vike_model::VenueRead::Balance(vike_model::VenueReadBalance {
            asset: "USDT".to_string(),
            balance: 7.5,
            allowances: Vec::new(),
            other_assets: Vec::new(),
        }))
    }
}

static READS: Reads = Reads { asked: std::sync::Mutex::new(Vec::new()) };
static READS_REG: [VenueAdapterRow; 1] = [VenueAdapterRow::Adapter(&READS)];

/// **The lane forwards an operator's read to the registry bridge** with the inputs of THE target (its tier
/// and its account's label), builds nothing and starts nothing; a bridge that implements none answers the
/// one `not offered` sentence; a venue this build carries no adapter for, or an account the wire cannot
/// spell, is an error in words, and the bridge is never asked.
#[test]
fn an_operators_read_reaches_the_bridge_with_the_targets_inputs_and_a_silent_bridge_is_not_offered()
{
    use vike_model::VenueReadKind::{Balance, OpenOrders};
    let policy = vike_tradehub_core::AdapterPolicy::default();
    let reading = lane(&READS_REG, &policy);
    let named = TradeLaneTarget {
        venue: "bybit".to_string(),
        account: Some("ALT".into()),
        tier: AccountTier::Live,
    };
    assert!(matches!(reading.venue_read(&named, Balance), Ok(vike_model::VenueRead::Balance(_))));
    assert!(reading.venue_read(&target("bybit", AccountTier::Demo), OpenOrders).is_ok());
    assert_eq!(
        *READS.asked.lock().expect("asked"),
        [
            (AccountTier::Live, Some("ALT".to_string()), Balance),
            (AccountTier::Demo, None, OpenOrders)
        ]
    );

    // A bridge that offers none: the trait default's sentence, byte for byte.
    let silent = lane(&MIXED_REG, &policy);
    assert_eq!(
        silent.venue_read(&target("binance", AccountTier::Demo), OpenOrders),
        Err(vike_model::not_offered_words("binance", OpenOrders).into())
    );
    // A venue this build does not carry, and a label the wire cannot spell: words, and no bridge asked.
    let absent = silent.venue_read(&target("ibkr", AccountTier::Demo), Balance).unwrap_err();
    assert!(absent.text().contains("this build carries no adapter"), "{absent}");
    let bad = TradeLaneTarget {
        account: Some("not a label".into()),
        ..target("bybit", AccountTier::Demo)
    };
    assert!(
        reading.venue_read(&bad, Balance).unwrap_err().text().contains("the account has no row")
    );
    assert_eq!(
        READS.asked.lock().expect("asked").len(),
        2,
        "the refusals never reached the bridge"
    );
}

/// **A Gamma READ that failed is "not now", a refusal of the NAME is a verdict** (polymarket's market-name
/// resolution): decided by the bridge's variant (`MarketNameRefusal`), never by the words, so a verdict that
/// opens with `network:` stays a verdict; the node remembers a definitive refusal for a while but asks again
/// after a failed read.
#[test]
fn a_failed_gamma_read_is_not_a_verdict_on_the_name() {
    use vike_polymarket::catalog::market_ref::MarketNameRefusal;
    for read in ["network: http status: 429", "a keyset page with no `data`"] {
        let said =
            super::bridge_trade::unavailable_if_unread(MarketNameRefusal::Unread(read.to_string()));
        assert_eq!(said.code, SymbolPropertiesRefusalCode::ListUnavailable, "{said}");
        assert!(said.reason.starts_with("polymarket: ") && said.reason.ends_with(read), "{said}");
    }
    for verdict in [
        "network: Gamma lists no market with the slug `network:Yes`; check the spelling",
        "polymarket: the market `rain` is CLOSED (resolved), so none of its tokens can be traded any more",
    ] {
        let said = super::bridge_trade::unavailable_if_unread(MarketNameRefusal::Refused(
            verdict.to_string(),
        ));
        assert_eq!(
            said,
            super::symbol_facts::verdict(verdict),
            "a verdict is said as the bridge said it, coded a verdict"
        );
    }
}

/// **The broker code's LENGTH reaches the node, the code never does** (decision 0270): a lane whose deployment rows
/// hold a Binance code answers the code's length, logs nothing holding it, and the reply the session builds with it
/// carries no part of it, in `Debug` or on the wire. A venue whose attribution is not an order client id prefix (bybit's is a
/// header) and a lane with no code answer `0`, the fit for no code.
#[test]
fn the_broker_code_length_leaves_this_process_and_the_code_never_does() {
    const CODE: &str = "Zx9SeCRet7";
    let vars = HashMap::from([
        ("BINANCE_BROKER_CODE".to_string(), CODE.to_string()),
        ("BYBIT_BROKER_CODE".to_string(), CODE.to_string()),
    ]);
    let policy = vike_tradehub_core::AdapterPolicy::default();
    let coded =
        TradeLaneBridge::with_snapshot(&MIXED_REG, vars, super::testutil::snapshot_of(&policy));
    let binance = target("binance", AccountTier::Demo);
    let (len, events) = vike_log::capture::captured(|| coded.broker_code_len(&binance, &start()));
    assert_eq!(usize::from(len), CODE.len());
    assert!(!format!("{events:?}").contains(CODE), "a log line held the code: {events:?}");
    let reply = vike_gatewayhub_client::proto::AdapterReply::Running {
        bound_tier: AccountTier::Demo,
        grid: None,
        contract_value: None,
        margin_mode: None,
        leg_grids: Vec::new(),
        reconcile: false,
        leverage_cap: None,
        account_currency: None,
        account_currency_assumed: false,
        native_time_release: false,
        broker_code_len: len,
    };
    assert!(!format!("{reply:?}").contains(CODE));
    assert!(!serde_json::to_string(&reply).expect("serialises").contains(CODE));
    assert_eq!(coded.broker_code_len(&target("bybit", AccountTier::Demo), &start()), 0);
    assert_eq!(lane(&MIXED_REG, &policy).broker_code_len(&binance, &start()), 0, "no code");
}

/// **A running account's listing is kept as its venue's held list, and an empty read is not** (decision 0062's
/// decision 3, amended 2026-11-13): an empty list would answer the datahub as a venue that lists nothing. The venue
/// key is this test's own: the slot is process-wide.
#[test]
fn a_listing_the_facts_read_is_held_for_the_kept_list_and_an_empty_one_is_not() {
    const VENUE: &str = "hold-listing-test";
    super::bridge_trade::hold_listing(VENUE, &[]);
    assert_eq!(vike_catalog::held_list::held(VENUE), None, "an empty read holds nothing");
    let row = vike_model::Instrument::new(
        VENUE,
        "EUR_USD",
        "EUR",
        "USD",
        vike_model::Contract::Fx(vike_model::ContractFx::default()),
    );
    super::bridge_trade::hold_listing(VENUE, std::slice::from_ref(&row));
    assert_eq!(vike_catalog::held_list::held(VENUE), Some(vec![row]));
}
