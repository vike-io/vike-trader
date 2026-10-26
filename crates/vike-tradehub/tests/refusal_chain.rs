//! **One refusal per family through the REAL hops** (wave 2, W12b): the venue's own refusal, raised inside
//! the gatewayhub's real venue client, crosses the real gatewayhub server, the real datahub strategy lane
//! and the tradehub's real `StratFeed`, and comes out of the REAL feed arm as the text an operator reads.
//!
//! # What is real, and what is scripted
//!
//! * the gatewayhub is `vike_gatewayhub::server::accept::serve` over the PRODUCTION data lane
//!   (`BridgeData::new(..).with_credentialed(..)`): the real oanda, IG and deribit clients are built by the
//!   real tables and refuse an interval BEFORE any network (their `subscribe_bars` checks the interval
//!   first), so no venue socket is ever dialled;
//! * the datahub is `serve_with_lanes` over a real `StratHub` and the real `FeedBroker` with
//!   `real_client_table` (the gatewayhub client the daemon builds), under a `Scope::Trade` key;
//! * the tradehub side is `ProdFeedCtors` + `wire_venue_feeds`, the production constructor and the arm;
//! * scripted: the credential store (an `AccountSource` holding fake demo credentials, never a real
//!   store or key) and the paper core the arm wires its sinks onto.
//!
//! It replaces what the plans used to pin before the bridge tables left the tradehub
//! (`oanda_plan_refuses_..._an_unservable_interval`, `ig_plan_refuses_..._an_unstreamable_interval`,
//! `deribit_plan_refuses_..._an_unservable_interval`): the refusal still happens, one hop further, and
//! this is the test that says what the operator reads.

use std::collections::{BTreeMap, HashSet};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use vike_data::{HistStore, LiveDataSink, MemHistStore};
use vike_datahub::feeds::FeedBroker;
use vike_datahub::feeds::venues::{real_client_table, sharing_for, strat_venues};
use vike_datahub::server::{ServerLanes, ServerTimings, serve_with_lanes};
use vike_datahub::strat::{StratHub, StratLimits};
use vike_gatewayhub::data::accounts::AccountSource;
use vike_gatewayhub::data::credentialed::CredentialedLane;
use vike_gatewayhub::data::venues::BridgeData;
use vike_gatewayhub::server::accept::serve;
use vike_gatewayhub::server::bind::bind_loopback;
use vike_gatewayhub::server::lanes::Lanes;
use vike_gatewayhub::server::limits::ServerConfig;
use vike_gatewayhub::testkit::gateway::keys;
use vike_gatewayhub::testkit::rest::ScriptedRest;
use vike_gatewayhub_client::client::GatewayLink;
use vike_hub_proto::auth::HubKeys;
use vike_model::AccountTier;
use vike_secrets::account_credentials::CredentialBook;
use vike_secrets::account_directory::AccountDirectory;
use vike_secrets::{Account, Accounts};
use vike_tradehub::feeds::{FeedCtors, ProdFeedCtors, venue_feed_plan, wire_venue_feeds};
use vike_tradehub_core::{
    MakerConfig, PaperCoreOpts, PaperHalt, StrategyMountSpec, build_paper_multi_strategy_core_with,
};

const TRADE_KEY: &[u8] = b"refusal-chain-strategy-lane-key";

/// Fake demo credentials for the credentialed venues, on the default accounts' rows (ids 1, 2, 3).
struct FakeAccounts(AccountDirectory);

impl AccountSource for FakeAccounts {
    fn directory(&self, _venue: &str) -> AccountDirectory {
        self.0.clone()
    }
}

fn account(id: i64, venue: &str) -> Account {
    Account {
        id,
        venue: venue.to_string(),
        tier: AccountTier::Demo,
        label: None,
        venue_account_id: None,
        parent_id: None,
        active: true,
        last_verified_at: None,
        max_exposure: None,
        leverage: None,
    }
}

fn fake_accounts() -> Arc<dyn AccountSource> {
    let mut book = CredentialBook::new();
    for (field, value) in [("CLIENT_ID", "fake"), ("CLIENT_SECRET", "fake"), ("ACCOUNT_ID", "fake")]
    {
        book.insert_account(1, field, value);
    }
    for (field, value) in [("API_KEY", "fake-ig-key"), ("IDENTIFIER", "fake"), ("PASSWORD", "fake")]
    {
        book.insert_account(2, field, value);
    }
    for (field, value) in [("API_KEY", "fake-oanda-token"), ("ACCOUNT_ID", "101-004-0000000-001")] {
        book.insert_account(3, field, value);
    }
    let rows = vec![account(1, "alpaca"), account(2, "ig"), account(3, "oanda")];
    Arc::new(FakeAccounts(
        AccountDirectory::from_rows(Accounts::Known(rows), None).with_credentials(book),
    ))
}

/// The whole stack under the tradehub: a gatewayhub over the production lane and a datahub over it.
struct Stack {
    datahub: String,
    stop: Arc<AtomicBool>,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

impl Stack {
    fn start() -> Self {
        Self::start_with(true)
    }

    /// The datahub UP over a gatewayhub that is NOT there (its address is a port nothing listens at): the mixed
    /// outage the feed gate's probe cannot see, because it asks the datahub only.
    fn start_without_gatewayhub() -> Self {
        Self::start_with(false)
    }

    fn start_with(gateway_up: bool) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::new();

        // The gatewayhub: the production data lane, credentialed streams included.
        let lane = BridgeData::new(BTreeMap::new()).with_credentialed(CredentialedLane::new(
            fake_accounts(),
            None,
            &BTreeMap::new(),
        ));
        let listener = bind_loopback("127.0.0.1:0").expect("bind the gatewayhub");
        let gateway = listener.local_addr().expect("addr").to_string();
        let lanes = Lanes { data: Arc::new(lane), rest: Arc::new(ScriptedRest::new()) };
        let flag = Arc::clone(&stop);
        if gateway_up {
            threads.push(
                std::thread::Builder::new()
                    .name("chain-gatewayhub".into())
                    .spawn(move || {
                        let _ = serve(listener, keys(), lanes, ServerConfig::PRODUCTION, flag);
                    })
                    .expect("spawn the gatewayhub"),
            );
        } else {
            // Closed: the port stays unlistened, so a dial is refused at once.
            drop((listener, lanes, flag));
        }

        // The datahub: the strategy lane over the broker the daemon builds (`real_client_table`).
        let link = GatewayLink::new(gateway, keys());
        let broker = FeedBroker::new(real_client_table(&link), sharing_for);
        let hub = StratHub::new(
            broker,
            strat_venues().into_iter().map(str::to_string).collect(),
            None,
            StratLimits::PRODUCTION,
        );
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind the datahub");
        let datahub = listener.local_addr().expect("addr").to_string();
        let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
        let hub_keys = HubKeys::new(b"chain-observe".to_vec(), b"chain-control".to_vec())
            .with_trade(TRADE_KEY.to_vec());
        let lanes = ServerLanes { strat: Some(hub), ..ServerLanes::default() };
        threads.push(
            std::thread::Builder::new()
                .name("chain-datahub".into())
                .spawn(move || {
                    let _ = serve_with_lanes(
                        listener,
                        store,
                        None,
                        Some(hub_keys),
                        None,
                        None,
                        lanes,
                        None,
                        false,
                        ServerTimings::PRODUCTION,
                    );
                })
                .expect("spawn the datahub"),
        );
        Self { datahub, stop, threads: Mutex::new(threads) }
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        // The gatewayhub's accept loop ends on the flag; the datahub's server is detached (it lives for
        // the test process), as in every other rig of this kind.
        self.stop.store(true, Ordering::SeqCst);
        let threads = std::mem::take(&mut *self.threads.lock().expect("threads"));
        drop(threads);
    }
}

/// The mount config of `venue` on its wired market, at `interval`.
fn cfg(venue: &str, symbol: &str, tick_size: f64, interval: &str, interval_ms: i64) -> MakerConfig {
    let mut cfg = MakerConfig::crypto(venue, symbol, tick_size, 1.0);
    cfg.interval = interval.to_string();
    cfg.interval_ms = interval_ms;
    cfg
}

/// Run the REAL arm for `cfg` over the stack and return the refusal the operator reads.
fn refusal_of(stack: &Stack, cfg: &MakerConfig) -> String {
    let plan = venue_feed_plan(
        cfg,
        &vike_tradehub_core::ArmingTable::default(),
        &vike_tradehub_core::AdapterPolicy::default(),
    )
    .unwrap_or_else(|e| {
        panic!(
            "{}: the plan passes this interval (the bridge's table is not its to read): {e}",
            cfg.venue
        )
    });
    let sentinel = std::env::temp_dir()
        .join(format!("vike-refusal-chain-{}-{}", cfg.venue, std::process::id()))
        .join("HALT");
    let strategy: Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send> =
        Box::new(vike_strategy::BuyHold::new(0.001, None));
    let mount = build_paper_multi_strategy_core_with(
        vec![StrategyMountSpec { strategy, spec: cfg.mount_spec() }],
        PaperCoreOpts { halt: PaperHalt::Pinned(sentinel), ..Default::default() },
    );
    let ctors = ProdFeedCtors::new(stack.datahub.clone(), Some(TRADE_KEY.to_vec()));
    let wrap = |sink: Arc<dyn LiveDataSink>| sink;
    let outcome = wire_venue_feeds(
        &plan,
        &[cfg],
        &mount.handle,
        &HashSet::new(),
        &[],
        &[vike_model::accounts::account_keys::AccountLabel::Default],
        &HashSet::new(),
        &wrap,
        &ctors,
    );
    mount.handle.shutdown_and_join();
    match outcome {
        Ok(_) => panic!("{}: a refused interval must refuse the mount", cfg.venue),
        Err(e) => e,
    }
}

/// **OANDA, an interval its candle table has no granularity for**: the real oanda client refuses `7m` at the
/// gatewayhub before any socket, and the words survive three hops and the arm.
#[test]
fn an_oanda_interval_the_bridge_cannot_serve_is_refused_with_the_bridges_words() {
    let stack = Stack::start();
    let err = refusal_of(&stack, &cfg("oanda", "EURUSD", 0.00001, "7m", 420_000));
    println!("oanda 7m, as the operator reads it: {err}");
    assert!(err.contains("no OANDA candle granularity"), "{err}");
    assert!(err.contains("EURUSD") && err.contains("7m"), "{err}");
}

/// **IG, an interval Lightstreamer has no scale for.**
#[test]
fn an_ig_interval_the_bridge_cannot_stream_is_refused_with_the_bridges_words() {
    let stack = Stack::start();
    let err = refusal_of(&stack, &cfg("ig", "CS.D.EURUSD.MINI.IP", 0.1, "15m", 900_000));
    println!("ig 15m, as the operator reads it: {err}");
    assert!(err.contains("ig streams no 15m candles"), "{err}");
}

/// **Deribit, an interval its `chart.trades` resolution table has a gap at** (no `4h`): refused at the
/// gatewayhub's `BarIntervalGate`, the tradehub's old `deribit_plan` text verbatim.
#[test]
fn a_deribit_interval_with_no_resolution_code_is_refused_with_the_old_plans_words() {
    let stack = Stack::start();
    let err = refusal_of(&stack, &cfg("deribit", "BTC-PERPETUAL", 0.5, "4h", 14_400_000));
    println!("deribit 4h, as the operator reads it: {err}");
    assert!(err.contains("there is no 4h") && err.contains("resolution_code"), "{err}");
}

/// **ALPACA keeps its rule where it always was, at the PLAN** (a string rule, not a bridge table: 1m bars
/// only). Polymarket's same-token and ctrader's same-venue interval gates are unit tests
/// (`feed_splice/feed_outage.rs`, `alpaca_ctrader.rs`): they run before any core spawns.
#[test]
fn alpacas_one_minute_rule_stays_at_the_plan() {
    let alpaca = cfg("alpaca", "AAPL", 0.01, "5m", 300_000);
    let err = venue_feed_plan(
        &alpaca,
        &vike_tradehub_core::ArmingTable::default(),
        &vike_tradehub_core::AdapterPolicy::default(),
    )
    .expect_err("alpaca serves 1m bars only");
    assert!(err.contains("1m"), "{err}");
}

/// **Datahub UP, gatewayhub DOWN: the first subscribe is refused as a TRANSIENT, the arm says so in the text the
/// start reads, and the start can survive it where nothing live runs** (review 17, finding 1, scenario A). The
/// feed gate's probe asks the datahub only and passes; the refusal comes from the datahub's own link to the
/// gatewayhub, crosses the real strategy lane and the real arm, and carries `TRANSIENT_MARK`, which the start reads
/// (`feed_gate::survivable_refusal`, unit-tested in `src/feed_gate_tests.rs`).
/// CONTROL below: a bridge's own refusal is not transient.
#[test]
fn a_datahub_up_over_a_gatewayhub_that_is_down_refuses_transiently_and_the_start_can_survive_it() {
    let stack = Stack::start_without_gatewayhub();
    let ctors = ProdFeedCtors::new(stack.datahub.clone(), Some(TRADE_KEY.to_vec()));
    // The probe (datahub only, plus the features it advertises for the mounted venue) passes.
    assert_eq!(
        ctors.datahub_probe(&["deribit".to_string()]).expect("production asks").check(),
        None,
        "the datahub is up and serves the lane for deribit: the probe cannot see the gatewayhub"
    );
    let err = refusal_of(&stack, &cfg("deribit", "BTC-PERPETUAL", 0.5, "1m", 60_000));
    println!("deribit over a down gatewayhub, as the start reads it: {err}");
    assert!(
        vike_market_stream::market::is_transient_refusal(&err),
        "a link refusal carries the transient mark: {err}"
    );
    assert!(err.contains("the link is down"), "{err}");
}

/// **CONTROL: a refusal that is the bridge's own answer is NOT transient** (a start must keep ending on a
/// bad interval), over the same stack with the gatewayhub up.
#[test]
fn a_bridges_own_refusal_is_not_transient() {
    let stack = Stack::start();
    let err = refusal_of(&stack, &cfg("deribit", "BTC-PERPETUAL", 0.5, "4h", 14_400_000));
    assert!(!vike_market_stream::market::is_transient_refusal(&err), "{err}");
}

/// **A datahub that cannot serve a mounted venue fails the PROBE, with its words, before any core**: the probe
/// reads the features the datahub advertised for every mounted venue, so an older or smaller build mid-deploy is a
/// held feed and not a start that dies at the first subscribe after its core is up. A venue it does serve passes.
#[test]
fn a_datahub_that_does_not_serve_a_mounted_venue_fails_the_probe_with_its_words() {
    let stack = Stack::start();
    let ctors = ProdFeedCtors::new(stack.datahub.clone(), Some(TRADE_KEY.to_vec()));
    assert_eq!(
        ctors.datahub_probe(&["deribit".to_string()]).expect("production asks").check(),
        None,
        "a venue the lane serves passes"
    );
    let why = ctors
        .datahub_probe(&["deribit".to_string(), "no-such-venue".to_string()])
        .expect("production asks")
        .check()
        .expect("a venue the datahub does not advertise fails the probe");
    assert!(
        why.contains("no-such-venue") && why.contains("serves no strategy lane"),
        "the words name the venue and the missing lane: {why}"
    );
}
