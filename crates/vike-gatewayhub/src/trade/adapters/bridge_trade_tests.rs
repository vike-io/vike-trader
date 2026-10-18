//! `BridgeTrade`'s own behaviour: what it adds to the moved venue half (the wire-shaped answers), over PLANTED
//! rows. The venue half's rules are `contract_tests/`, `exclusive_*tests` and `startup_tests/`; the real
//! bridges' arming is `tests/trade_roster/`.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};

use vike_bridge_core::venue_adapter::{
    AdapterInputs, AdapterOutcome, AdapterRequest, ClockAuth, ClockDecl, ClockRisk, PaperCause,
    Resolution, VenueAdapter, VenueDeclaration,
};
use vike_bridge_core::venue_adapter_fixture::{PLANTED_DECLARATION, PlantedAdapter};
use vike_config::ArmingBlock;
use vike_gatewayhub_client::proto::{
    AdapterStart, ClockLeg, ClockRiskClass, CredentialLeg, LegResult, NetReading as WireNet,
    PreflightStep,
};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;

use super::bridge_trade::BridgeTrade;
use super::probe::{CredentialGap, NetReading, clock_result, credential_result, network_result};
use super::registry::VenueRow;
use crate::trade::lane::{TradeLane, TradeTarget};

/// A venue that is present, resolves paper for want of credentials and counts how often its bridge was asked
/// to speak for an account that will not start.
struct Speaks {
    spoke: AtomicUsize,
}

impl VenueAdapter for Speaks {
    fn venue(&self) -> &'static str {
        "bybit"
    }
    fn declaration(&self) -> VenueDeclaration {
        PLANTED_DECLARATION
    }
    fn resolve(&self, _inputs: &AdapterInputs<'_>) -> Resolution {
        Resolution::Paper(PaperCause::NoCredentials)
    }
    fn start(&self, _req: AdapterRequest<'_>) -> AdapterOutcome {
        AdapterOutcome::paper()
    }
    fn report_not_running_account(&self, _inputs: &AdapterInputs<'_>) {
        self.spoke.fetch_add(1, Ordering::SeqCst);
    }
}

static SPEAKS: Speaks = Speaks { spoke: AtomicUsize::new(0) };
static SPEAKS_REG: [VenueRow; 1] = [VenueRow::Adapter(&SPEAKS)];

static PLAIN: PlantedAdapter =
    PlantedAdapter::new("binance", Resolution::Paper(PaperCause::NoCredentials));
static MIXED_REG: [VenueRow; 2] =
    [VenueRow::Adapter(&PLAIN), VenueRow::FeatureAbsent { venue: "ibkr", feature: "ibkr" }];

fn lane(registry: &'static [VenueRow], policy: &vike_tradehub_core::AdapterPolicy) -> BridgeTrade {
    BridgeTrade::with_snapshot(registry, HashMap::new(), super::testutil::snapshot_of(policy))
}

fn start() -> AdapterStart {
    AdapterStart {
        rid: 1,
        symbol: "BTCUSDT".to_string(),
        declared_legs: Vec::new(),
        recon_enabled: false,
        takes_trigger: false,
        risk: None,
        market_slippage: None,
        halt_admit: vike_model::HaltAdmit::Admit,
        withhold: None,
        route_key: "binance".to_string(),
    }
}

fn target(venue: &str, tier: AccountTier) -> TradeTarget {
    TradeTarget { venue: venue.to_string(), account: None, tier }
}

fn events() -> vike_exec::EventSender {
    vike_exec::event_channel(8).0
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
fn a_start_forgets_the_accounts_kept_leverage_caps() {
    let policy = vike_tradehub_core::AdapterPolicy::default();
    let lane = lane(&MIXED_REG, &policy);
    let reads = std::cell::Cell::new(0);
    let ask = || {
        lane.leverage_caps_for_test()
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
const fn wired(risk: ClockRisk) -> VenueDeclaration {
    VenueDeclaration {
        clock: ClockDecl::Wired { endpoint: "planted", auth: ClockAuth::Public, risk },
        ..PLANTED_DECLARATION
    }
}

static SIGNED: PlantedAdapter = PlantedAdapter {
    declaration: wired(ClockRisk::SignedTimestamp),
    server_time: Some(|_| Ok(1_700_000_000_123)),
    ..PlantedAdapter::new("binance", Resolution::Paper(PaperCause::NoCredentials))
};
static NONCE: PlantedAdapter = PlantedAdapter {
    declaration: wired(ClockRisk::NonceWindow),
    server_time: Some(|_| Err("timed out".to_string())),
    ..PlantedAdapter::new("hyperliquid", Resolution::Paper(PaperCause::NoCredentials))
};
static NO_CLOCK: PlantedAdapter = PlantedAdapter {
    declaration: VenueDeclaration {
        clock: ClockDecl::NotWired { reason: "ig publishes no clock", unmeasured_risk: None },
        ..PLANTED_DECLARATION
    },
    ..PlantedAdapter::new("ig", Resolution::Paper(PaperCause::NoCredentials))
};
static RISKY: PlantedAdapter = PlantedAdapter {
    declaration: VenueDeclaration {
        clock: ClockDecl::NotWired {
            reason: "polymarket publishes no clock",
            unmeasured_risk: Some("its signature binds the timestamp into every order"),
        },
        ..PLANTED_DECLARATION
    },
    ..PlantedAdapter::new("polymarket", Resolution::Paper(PaperCause::NoCredentials))
};
static CLOCK_REG: [VenueRow; 4] = [
    VenueRow::Adapter(&SIGNED),
    VenueRow::Adapter(&NONCE),
    VenueRow::Adapter(&NO_CLOCK),
    VenueRow::Adapter(&RISKY),
];

fn clock_lane() -> BridgeTrade {
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
        matches!(&legs[0], LegResult::Clock { venue, leg: ClockLeg::NotChecked, reason: Some(why), .. }
            if venue == "binance" && why.contains("no account row states its tier")),
        "{legs:?}"
    );
    assert!(matches!(legs.last(), Some(LegResult::Network { .. })), "{legs:?}");
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
    assert_eq!(lists.clock_risks["binance"].class, ClockRiskClass::SignedTimestamp);
    assert_eq!(lists.clock_risks["binance"].remedy, ClockRisk::SignedTimestamp.remedy());
    assert_eq!(lists.clock_risks["hyperliquid"].class, ClockRiskClass::NonceWindow);
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
        LegResult::Clock { venue, leg: ClockLeg::ServerTime { server_ms: 1_700_000_000_123, .. }, reason: None, .. }
            if venue == "binance"
    ));
    assert!(matches!(
        &legs[1],
        LegResult::Clock { leg: ClockLeg::Unreachable(why), .. } if why.contains("timed out")
    ));
    assert!(matches!(
        &legs[2],
        LegResult::Clock { leg: ClockLeg::NotChecked, reason: Some(why), at_stake: None, .. }
            if why == "ig publishes no clock"
    ));
    assert!(matches!(
        &legs[3],
        LegResult::Clock { leg: ClockLeg::UnmeasuredRisk, reason: Some(why), at_stake: Some(cost), .. }
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
        matches!(&legs[..], [LegResult::Credential { venue, label: None, leg: CredentialLeg::Rejected(why) }]
            if venue == "binance" && !why.contains("was planned")),
        "{legs:?}"
    );
}

/// The network question answers ONE network leg whatever venues it names.
#[test]
fn a_network_question_answers_one_network_leg() {
    let legs = clock_lane().preflight(&[], PreflightStep::Network).legs;
    assert!(matches!(&legs[..], [LegResult::Network { .. }]), "{legs:?}");
}

/// **The wire's network leg carries the numbers the report prints**, and the clock and credential legs
/// carry what the originals said (one mapping, shared by the lane and every scripted double).
#[test]
fn the_wire_legs_carry_the_probes_words_and_numbers() {
    assert_eq!(
        network_result(NetReading::Alive { rounds: 7 }),
        LegResult::Network { reading: WireNet::Alive, rounds: Some(7), since_ms: None }
    );
    assert!(matches!(
        network_result(NetReading::Dead { since_ms: 42 }),
        LegResult::Network { reading: WireNet::Dead(_), rounds: None, since_ms: Some(42) }
    ));
    assert_eq!(
        network_result(NetReading::Unknown),
        LegResult::Network { reading: WireNet::Unknown, rounds: None, since_ms: None }
    );
    assert_eq!(
        credential_result(
            "okx",
            None,
            Err(CredentialGap::Unanswered { waited_ms: 5, detail: "d".into() })
        ),
        LegResult::Credential {
            venue: "okx".into(),
            label: None,
            leg: CredentialLeg::Unanswered { waited_ms: 5, detail: "d".into() }
        }
    );
    assert_eq!(
        clock_result("binance", Ok(10), 12, 4),
        LegResult::Clock {
            venue: "binance".into(),
            leg: ClockLeg::ServerTime { server_ms: 10, local_ms: 12, rtt_ms: 4 },
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
    let a = BridgeTrade::new(&MIXED_REG, HashMap::new(), &policy.accounts, &BTreeMap::new());
    let b = lane(&MIXED_REG, &policy);
    assert_eq!(a.arming_table_quiet(), b.arming_table_quiet());
}

// ---- the supervised link (decision 0164 verdict 7(b)) -------------------------------------------------------

mod link_watch {
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use vike_secrets::account_directory::AccountDirectory;
    use vike_secrets::venue_setting::VenueSettings;
    use vike_secrets::{Account, Accounts, VenueSettingRow};

    use super::*;

    fn ibkr_row(id: i64, tier: AccountTier, active: bool) -> Account {
        Account {
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
        }
    }

    fn directory(rows: Vec<Account>) -> AccountDirectory {
        AccountDirectory::from_rows(Accounts::Known(rows), None)
    }

    fn gateway_rows(port: u16) -> BTreeMap<String, VenueSettings> {
        let rows = vec![
            VenueSettingRow {
                venue: "ibkr".into(),
                tier: Some(AccountTier::Demo),
                field: "HOST".into(),
                value: "127.0.0.1".into(),
            },
            VenueSettingRow {
                venue: "ibkr".into(),
                tier: Some(AccountTier::Demo),
                field: "PORT".into(),
                value: port.to_string(),
            },
        ];
        BTreeMap::from([("ibkr".to_string(), VenueSettings::from_rows("ibkr", &rows))])
    }

    fn lane_over(
        accounts: &AccountDirectory,
        settings: &BTreeMap<String, VenueSettings>,
    ) -> BridgeTrade {
        BridgeTrade::new(&super::SPEAKS_REG, HashMap::new(), accounts, settings)
    }

    /// Only IBKR's gateway is supervised: every other bridge owns its reconnect, and the daemon adds none.
    #[test]
    fn only_ibkr_is_supervised() {
        let lane = lane_over(&directory(vec![]), &gateway_rows(1));
        assert!(lane.link_watch(&target("binance", AccountTier::Demo), &start()).is_none());
        assert!(lane.link_watch(&target("ibkr", AccountTier::Demo), &start()).is_some());
    }

    /// The watch probes the port the tier's rows name and asks the rows AGAIN at every call, through the
    /// fresh-read source when the daemon built one.
    #[test]
    fn the_watch_probes_the_tiers_port_and_reads_the_rows_fresh() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("port").port();
        let active = Arc::new(AtomicBool::new(true));
        let reads = Arc::new(AtomicUsize::new(0));
        let rows: super::super::bridge_trade::AccountRows = {
            let (active, reads) = (Arc::clone(&active), Arc::clone(&reads));
            Arc::new(move || {
                reads.fetch_add(1, Ordering::SeqCst);
                directory(vec![ibkr_row(1, AccountTier::Demo, active.load(Ordering::SeqCst))])
            })
        };
        // The snapshot the lane started with says the account is active and never changes.
        let started = directory(vec![ibkr_row(1, AccountTier::Demo, true)]);
        let lane = lane_over(&started, &gateway_rows(port)).with_account_rows(rows);
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
    #[test]
    fn without_a_fresh_source_the_snapshot_answers() {
        let started = directory(vec![ibkr_row(1, AccountTier::Demo, true)]);
        let lane = lane_over(&started, &gateway_rows(1));
        let watch = lane.link_watch(&target("ibkr", AccountTier::Demo), &start()).expect("watched");
        assert_eq!(watch.tier_now(), Ok(AccountTier::Demo));
    }

    /// The venue-scoped two-tier conflict of decision 0140 (a demo row beside a live row) is read through
    /// `account_tier`, so it stops the watch's predicate exactly as it stops an arm.
    #[test]
    fn the_two_tier_conflict_is_the_predicates_answer() {
        let rows = directory(vec![
            ibkr_row(1, AccountTier::Demo, true),
            ibkr_row(2, AccountTier::Live, true),
        ]);
        let lane = lane_over(&rows, &gateway_rows(1));
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
        let lane = lane_over(&directory(vec![]), &settings);
        assert!(lane.link_watch(&target("ibkr", AccountTier::Demo), &start()).is_none());
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
    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration { addresses_accounts: true, ..PLANTED_DECLARATION }
    }
    fn resolve(&self, _inputs: &AdapterInputs<'_>) -> Resolution {
        Resolution::Armed { tier: AccountTier::Demo, held_below_live: None }
    }
    fn start(&self, _req: AdapterRequest<'_>) -> AdapterOutcome {
        self.started.fetch_add(1, Ordering::SeqCst);
        AdapterOutcome::paper()
    }
}

static COUNTING: Counting = Counting { started: AtomicUsize::new(0) };
static COUNTING_REG: [VenueRow; 1] = [VenueRow::Adapter(&COUNTING)];

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
    let unread = BridgeTrade::with_snapshot(&COUNTING_REG, HashMap::new(), snapshot);
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

static ALPACA: PlantedAdapter =
    PlantedAdapter::new("alpaca", Resolution::Paper(PaperCause::NoCredentials));
static ALPACA_REG: [VenueRow; 1] = [VenueRow::Adapter(&ALPACA)];

/// A lane whose PUBLIC list is a trap: alpaca's facts must never be asked of it.
fn facts_lane(registry: &'static [VenueRow]) -> BridgeTrade {
    use super::symbol_facts::SymbolFacts;
    lane(registry, &vike_tradehub_core::AdapterPolicy::default()).with_symbol_facts(
        std::sync::Arc::new(SymbolFacts::new(std::sync::Arc::new(|venue| {
            Err(format!("{venue}: the public list must not be asked for"))
        }))),
    )
}

/// **alpaca's facts are the account's own listing, never the public one**: a `live` account is refused
/// (the adapter trades its sandbox only), an account with no credentials lists nothing and says so (no
/// request leaves), and a build with no alpaca row answers the default.
#[test]
fn alpaca_symbol_facts_come_from_the_accounts_own_listing() {
    let lane = facts_lane(&ALPACA_REG);
    let live = lane.symbol_facts(&target("alpaca", AccountTier::Live), "AAPL").expect_err("live");
    assert!(live.contains("sandbox only"), "{live}");
    let none = lane.symbol_facts(&target("alpaca", AccountTier::Demo), "AAPL").expect_err("no key");
    assert!(
        none.contains("CLIENT_ID") && !none.contains("public list"),
        "no credentials: refused by field name, asked of nothing: {none}"
    );
    assert!(
        vike_gatewayhub_client::proto::facts_refusal_is_transient(&none),
        "a missing credential is not a verdict on the symbol: {none}"
    );
    let absent = facts_lane(&SPEAKS_REG)
        .symbol_facts(&target("alpaca", AccountTier::Demo), "AAPL")
        .expect_err("no alpaca row");
    assert_eq!(absent, super::symbol_facts::NOT_ANSWERED);
    // Another venue still reads the public list (the trap above answers for it).
    let other =
        lane.symbol_facts(&target("bybit", AccountTier::Demo), "ETHUSDT").expect_err("trap");
    assert!(other.contains("public list must not be asked for"), "{other}");
}

static CTRADER: PlantedAdapter =
    PlantedAdapter::new("ctrader", Resolution::Paper(PaperCause::NoCredentials));
static CTRADER_REG: [VenueRow; 1] = [VenueRow::Adapter(&CTRADER)];

/// **ctrader's facts are the account's own session listing, never the public one**: a `live` or `paper`
/// account is refused (the adapter dials the demo host alone), an account with no credentials lists
/// nothing and says so (no session opens), and a build with no ctrader row answers the default.
#[test]
fn ctrader_symbol_facts_come_from_the_accounts_own_session() {
    let lane = facts_lane(&CTRADER_REG);
    for tier in [AccountTier::Live, AccountTier::Paper] {
        let why = lane.symbol_facts(&target("ctrader", tier), "EURUSD").expect_err("not demo");
        assert!(why.contains("demo account only"), "{tier:?}: {why}");
    }
    let none =
        lane.symbol_facts(&target("ctrader", AccountTier::Demo), "EURUSD").expect_err("no key");
    assert!(
        none.contains("no usable credentials") && !none.contains("public list"),
        "no credentials: nothing to authenticate with, the public list is never asked: {none}"
    );
    let absent = facts_lane(&SPEAKS_REG)
        .symbol_facts(&target("ctrader", AccountTier::Demo), "EURUSD")
        .expect_err("no ctrader row");
    assert_eq!(absent, super::symbol_facts::NOT_ANSWERED);
}

static IG: PlantedAdapter = PlantedAdapter::new("ig", Resolution::Paper(PaperCause::NoCredentials));
static IG_REG: [VenueRow; 1] = [VenueRow::Adapter(&IG)];

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
        assert!(why.contains("demo gateway only"), "{tier:?}: {why}");
    }
    let none = lane.symbol_facts(&target("ig", AccountTier::Demo), EPIC).expect_err("no key");
    assert!(
        none.contains("no usable credentials") && !none.contains("public list"),
        "no credentials: refused before a login, asked of nothing: {none}"
    );
    let absent = facts_lane(&SPEAKS_REG)
        .symbol_facts(&target("ig", AccountTier::Demo), EPIC)
        .expect_err("no ig row");
    assert_eq!(absent, super::symbol_facts::NOT_ANSWERED);
}

#[cfg(feature = "ibkr")]
static IBKR: PlantedAdapter =
    PlantedAdapter::new("ibkr", Resolution::Paper(PaperCause::NoCredentials));
#[cfg(feature = "ibkr")]
static IBKR_REG: [VenueRow; 1] = [VenueRow::Adapter(&IBKR)];

/// **ibkr's facts come from the account's own gateway, never the public list**: a tier other than `demo` is
/// refused by name, an account with no `ACCOUNT` field looks nothing up (nothing is dialled), a build with no
/// ibkr row answers the default. A name the grammar refuses is refused before any dial.
#[cfg(feature = "ibkr")]
#[test]
fn ibkr_symbol_facts_come_from_the_accounts_own_gateway() {
    let lane = facts_lane(&IBKR_REG);
    let live = lane.symbol_facts(&target("ibkr", AccountTier::Live), "AAPL.SMART.USD");
    assert!(live.expect_err("live").contains("`demo` account only"));
    let none = lane
        .symbol_facts(&target("ibkr", AccountTier::Demo), "AAPL.SMART.USD")
        .expect_err("no account field");
    assert!(none.contains("no usable ACCOUNT"), "{none}");
    assert!(!none.contains("public list"), "{none}");
    let absent = facts_lane(&SPEAKS_REG)
        .symbol_facts(&target("ibkr", AccountTier::Demo), "AAPL.SMART.USD")
        .expect_err("no ibkr row");
    assert_eq!(absent, super::symbol_facts::NOT_ANSWERED);
}

static DUKASCOPY: PlantedAdapter =
    PlantedAdapter::new("dukascopy", Resolution::Paper(PaperCause::NoCredentials));
static DUKASCOPY_REG: [VenueRow; 1] = [VenueRow::Adapter(&DUKASCOPY)];

/// **dukascopy's facts are the committed SDK table's, never the public list and never a dial**: the grid of
/// a USD-quoted pair comes back whatever the (trap) public list would say, another quote currency, a CFD and
/// a name the table lacks are each refused by name, a tier other than `demo` is refused, and a build with no
/// dukascopy row answers the default.
#[test]
fn dukascopy_symbol_facts_come_from_the_committed_sdk_table() {
    let lane = facts_lane(&DUKASCOPY_REG);
    let demo = |symbol: &str| lane.symbol_facts(&target("dukascopy", AccountTier::Demo), symbol);
    let facts = demo("EURUSD").expect("a USD-quoted pair");
    assert_eq!(facts.properties.tick_size, 0.00001);
    assert_eq!((facts.properties.step_size, facts.properties.min_qty), (1000.0, 1000.0));
    assert_eq!(facts.contract_value, None, "linear: a unit of quantity is a unit of the base");
    assert!(demo("USDJPY").expect_err("quote").contains("quoted in JPY"));
    assert!(demo("AAPLUSUSD").expect_err("cfd").contains("blanket default"));
    assert!(demo("GBPNOK").expect_err("absent").contains("not one of the"));
    for tier in [AccountTier::Live, AccountTier::Paper] {
        let why = lane.symbol_facts(&target("dukascopy", tier), "EURUSD").expect_err("tier");
        assert!(why.contains("`demo` account only"), "{tier:?}: {why}");
    }
    let absent = facts_lane(&SPEAKS_REG)
        .symbol_facts(&target("dukascopy", AccountTier::Demo), "EURUSD")
        .expect_err("no dukascopy row");
    assert_eq!(absent, super::symbol_facts::NOT_ANSWERED);
}
