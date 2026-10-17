//! W12b: a price feed that cannot be had is NOT RUNNING with its reason, never a refused start; a feed that
//! REFUSES tears the core it found down.
//!
//! Before wave 2 the CEX, Hyperliquid, deribit and polymarket feeds were in-process, so a datahub or a
//! gatewayhub that was down changed nothing about a live-gate start. Now every feed is a dial to the
//! datahub's strategy lane, and the first `subscribe_*` against a lane that is not there used to end the
//! whole start (`ExitCode::FAILURE`) after the core, the restore and the leases were up. These tests drive
//! the REAL [`live_runtime_with`] (plan gate, withhold, `build_live_multi_strategy_core`, the feed gate, the
//! wire arms) over a scripted backend and a constructor that is away.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

use vike_gatewayhub_client::client::GatewayLink;

use crate::feeds::DatahubProbe;
use crate::late_arm::LateArmPlan;

/// A constructor whose datahub probe answers `down` and which COUNTS every dial: an away datahub must be
/// found by the probe, before anything dials.
struct AwayCtors {
    down: Option<String>,
    dials: Arc<AtomicUsize>,
}

impl AwayCtors {
    fn away(why: &str) -> (Self, Arc<AtomicUsize>) {
        let dials = Arc::new(AtomicUsize::new(0));
        (Self { down: Some(why.to_string()), dials: Arc::clone(&dials) }, dials)
    }
    fn up() -> Self {
        Self { down: None, dials: Arc::new(AtomicUsize::new(0)) }
    }
}

impl FeedCtors for AwayCtors {
    fn strat(
        &self,
        venue: &str,
        _network: Option<&str>,
        _sink: Arc<dyn LiveDataSink>,
    ) -> Result<vike_datahub_client::strat::StratFeed, String> {
        self.dials.fetch_add(1, Ordering::SeqCst);
        Err(format!("{venue}: no datahub in this test"))
    }

    fn client(
        &self,
        venue: &str,
        _network: Option<&str>,
        _sink: Arc<dyn LiveDataSink>,
    ) -> Result<Box<dyn DataClient + Send>, String> {
        self.dials.fetch_add(1, Ordering::SeqCst);
        Err(format!("{venue}: no datahub in this test"))
    }

    fn datahub_probe(&self, _venues: &[String]) -> Option<DatahubProbe> {
        let down = self.down.clone();
        Some(DatahubProbe::new(move || down.clone()))
    }
}

/// The in-process backend plus what a `RemoteCtor` adds: the late-arm plan over a link that is never
/// dialled (nothing here polls).
struct WaitingCtor;

impl BackendCtor for WaitingCtor {
    fn backend(
        &self,
        vars: &HashMap<String, String>,
        policy: &vike_tradehub_core::AdapterPolicy,
        data_only: &HashSet<String>,
    ) -> Arc<dyn vike_tradehub_core::VenueBackend> {
        BackendCtor::backend(&LocalCtor, vars, policy, data_only)
    }

    fn late_arm(
        &self,
        not_running: &[vike_tradehub_core::NotRunning],
        datahub: Option<DatahubProbe>,
    ) -> Option<LateArmPlan> {
        let waiting: Vec<String> = not_running
            .iter()
            .filter(|n| n.block == vike_config::ArmingBlock::GatewayUnavailable)
            .map(vike_tradehub_core::NotRunning::route_key)
            .collect();
        LateArmPlan::of(
            GatewayLink::new(
                "127.0.0.1:1",
                vike_hub_proto::auth::HubKeys::new(Vec::new(), Vec::new()),
            ),
            waiting,
            datahub,
        )
    }

    fn notice_plan(&self, why: String) -> Option<LateArmPlan> {
        Some(LateArmPlan::notice(
            GatewayLink::new(
                "127.0.0.1:1",
                vike_hub_proto::auth::HubKeys::new(Vec::new(), Vec::new()),
            ),
            why,
        ))
    }
}

/// Binance's default account on a `paper` row (the one the strategy mounts on) beside a SECOND binance account,
/// `ALT`, at `demo` holding a fake credential (so it WOULD arm: the scripted table arms a `demo` account that
/// holds any). A strategy mount on an account that does not run refuses the node (decision 0156), so the
/// account the outage holds back is one NO mount trades: the mixed box the late arm exists for.
fn paper_mount_and_a_demo_account_beside_it() -> super::SeamPolicy {
    let alt = vike_model::accounts::account_keys::AccountLabel::parse("ALT").expect("a label");
    let planted = vike_tradehub_core::AdapterPolicy::default()
        .with_account(
            "binance",
            &vike_model::accounts::account_keys::AccountLabel::Default,
            vike_model::AccountTier::Paper,
        )
        .with_account("binance", &alt, vike_model::AccountTier::Demo);
    let mut book = vike_secrets::account_credentials::CredentialBook::new();
    book.insert_account(2, "API_KEY", "fake-binance-key");
    book.insert_account(2, "API_SECRET", "fake-binance-secret");
    super::SeamPolicy {
        policy: vike_config::Policy::default(),
        accounts: planted.accounts.with_credentials(book),
    }
}

fn binance_mount() -> Vec<ResolvedAdapter> {
    vec![super::buy_hold_mount("binance", false)]
}

/// **A datahub that is away leaves the live accounts NOT RUNNING with a reason, and the start survives.**
/// The `demo` binance `ALT` account would run (the control below proves it: without the outage the same mount
/// reaches the live-intent budget refusal), but its price feed cannot be had, so the table says
/// `price feeds unavailable (datahub)` for it (the cause is the PRICE path; `gatewayhub unavailable` would send the operator to a healthy daemon), the `paper` binance account is untouched, no feed is wired and nothing
/// is dialled, and the late arm's plan waits for the datahub alone (the gatewayhub is not what is away).
#[test]
fn a_datahub_that_is_away_holds_the_live_account_not_running_and_the_start_survives() {
    // CONTROL: the datahub answers, no budget, so the demo binance ALT account is refused at the live-intent
    // gate. Without this the assertions below could pass against a mount that never armed anything.
    let lock_dir = tempfile::tempdir().expect("a throwaway state dir");
    let err = super::seam_live_runtime_over(
        binance_mount(),
        None,
        &paper_mount_and_a_demo_account_beside_it(),
        HashMap::new(),
        lock_dir.path(),
        &AwayCtors::up(),
        &WaitingCtor,
    )
    .err()
    .expect("a demo account with no risk budget refuses the mount");
    assert!(
        err.contains("max_notional_per_order") || err.contains("max_total_exposure"),
        "the control reached the live arms: {err}"
    );

    let (ctors, dials) = AwayCtors::away("datahub 127.0.0.1:1: strategy lane: connection refused");
    let (handle, teardown, live_venues, late_arm, not_running) = super::seam_live_runtime_over(
        binance_mount(),
        None,
        &paper_mount_and_a_demo_account_beside_it(),
        HashMap::new(),
        lock_dir.path(),
        &ctors,
        &WaitingCtor,
    )
    .expect("an away datahub is not a refused start");

    let stopped: Vec<(&str, &str)> = not_running
        .iter()
        .filter(|n| n.venue == "binance")
        .map(|n| (n.route_key.as_str(), n.reason.as_str()))
        .collect();
    assert_eq!(
        stopped,
        [("binance#ALT", "price feeds unavailable (datahub)")],
        "the demo account is NOT RUNNING with the arming word; the paper account runs"
    );
    assert!(
        !live_venues.contains("binance#ALT"),
        "no exec client was built for it: {live_venues:?}"
    );
    assert!(teardown.feeds.is_empty(), "no feed is wired while the datahub is away");
    assert_eq!(dials.load(Ordering::SeqCst), 0, "nothing dialled: the probe found it");
    let plan = late_arm.expect("an account and the feeds are waiting: the late arm has a plan");
    // The held account waits for the PRICE FEEDS (its own word says so), not for the gatewayhub: nothing is
    // wrong with the gatewayhub, so the late arm does not ask it and the plan is the datahub's alone.
    assert!(!plan.gateway, "no account is NOT RUNNING for want of the gatewayhub");
    assert!(plan.datahub.is_some(), "the feeds wait for the datahub");
    assert_eq!(plan.waiting, ["price feeds (datahub)"], "{:?}", plan.waiting);
    assert!(
        plan.feeds_why.as_deref().is_some_and(|why| why.contains("connection refused")),
        "the reason travels to the banner: {:?}",
        plan.feeds_why
    );
    super::tear_down(handle, teardown);
}

/// **A live-gate start whose every account is `paper` starts without the datahub** (it used to refuse): no
/// account is held, no feed is wired, and the late arm waits for the datahub ALONE (`gateway == false`, so a
/// box with no gatewayhub at all is not waited on for one).
#[test]
fn an_all_paper_box_starts_without_a_datahub_and_waits_for_it_alone() {
    let (ctors, dials) = AwayCtors::away("datahub 127.0.0.1:1: strategy lane: connection refused");
    let lock_dir = tempfile::tempdir().expect("a throwaway state dir");
    let (handle, teardown, _live, late_arm, not_running) = super::seam_live_runtime_over(
        vec![super::buy_hold_mount("deribit", false)],
        None,
        &super::SeamPolicy::armed(&[("deribit", vike_model::AccountTier::Paper)]),
        HashMap::new(),
        lock_dir.path(),
        &ctors,
        &WaitingCtor,
    )
    .expect("a paper box does not need the datahub to start");
    assert!(
        not_running.iter().all(|n| !n.reason.contains("unavailable")),
        "no account is held when every account is paper: {not_running:?}"
    );
    assert!(teardown.feeds.is_empty());
    assert_eq!(dials.load(Ordering::SeqCst), 0);
    let plan = late_arm.expect("the feeds wait for the datahub");
    assert!(!plan.gateway && plan.datahub.is_some(), "the datahub alone: {plan:?}");
    super::tear_down(handle, teardown);
}

/// **No strategy-lane key is the same outage, by name, with nothing dialled** — through the REAL production
/// constructor, not a script: the key store holds none, the probe says so before any socket, and the paper
/// box starts.
#[test]
fn an_absent_strategy_lane_key_is_the_same_wait_and_names_the_key() {
    let ctors = crate::feeds::ProdFeedCtors::new("127.0.0.1:1", None);
    let why = ctors.datahub_probe(&[]).expect("production asks").check().expect("no key, no lane");
    assert!(
        why.contains("holds no strategy-lane key") && why.contains("Nothing was dialled"),
        "{why}"
    );
    let lock_dir = tempfile::tempdir().expect("a throwaway state dir");
    let (handle, teardown, _live, late_arm, _nr) = super::seam_live_runtime_over(
        vec![super::buy_hold_mount("deribit", false)],
        None,
        &super::SeamPolicy::armed(&[("deribit", vike_model::AccountTier::Paper)]),
        HashMap::new(),
        lock_dir.path(),
        &ctors,
        &WaitingCtor,
    )
    .expect("a paper box starts without a key");
    assert!(teardown.feeds.is_empty());
    assert!(late_arm.is_some_and(|p| p.datahub.is_some()));
    super::tear_down(handle, teardown);
}

/// A datahub nothing listens at: the production probe dials once, fails, and says which address and that it
/// was the strategy lane — the words the NOT RUNNING line carries.
#[test]
fn a_datahub_nothing_listens_at_is_found_by_the_production_probe() {
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        l.local_addr().expect("addr").to_string()
    };
    let ctors = crate::feeds::ProdFeedCtors::new(dead.clone(), Some(b"a-trade-key".to_vec()));
    let why = ctors.datahub_probe(&[]).expect("production asks").check().expect("nothing listens");
    assert!(why.contains(&dead) && why.contains("strategy lane"), "{why}");
}

/// The kernel's `PF_EXITING` task flag: set when a task enters `do_exit`, before its tid word is cleared.
#[cfg(target_os = "linux")]
const PF_EXITING: u64 = 0x4;

/// The `vt-core` threads of this process that are still ALIVE (Linux: the only platform tests run on). A
/// thread that has been joined can linger in `/proc` as a zombie for a moment, and is not alive.
#[cfg(target_os = "linux")]
fn live_core_threads() -> HashSet<String> {
    std::fs::read_dir("/proc/self/task")
        .map(|d| {
            d.filter_map(Result::ok)
                .filter(|t| {
                    std::fs::read_to_string(t.path().join("comm"))
                        .is_ok_and(|n| n.trim() == "vt-core")
                })
                .filter(|t| {
                    // `/proc/<pid>/task/<tid>/stat` is `pid (comm) STATE ppid pgrp session tty tpgid FLAGS ...`
                    // after the last `)`. `Z` (zombie) and `X` (dead) are not running, and neither is a task
                    // with `PF_EXITING` (0x4) set: `join` returns when the kernel clears the tid word, in
                    // `do_exit`, so a joined thread can still be listed, not yet `Z`, for as long as it is
                    // descheduled (seen on a starved CI box), while one still in user code never has the flag.
                    std::fs::read_to_string(t.path().join("stat")).is_ok_and(|s| {
                        s.rsplit_once(')').is_some_and(|(_, rest)| {
                            let mut f = rest.split_whitespace();
                            let state = f.next().and_then(|s| s.chars().next());
                            let flags = f.nth(5).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
                            !matches!(state, Some('Z' | 'X')) && flags & PF_EXITING == 0
                        })
                    })
                })
                .map(|t| t.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// A constructor whose feed FLOODS the core's lossless tick lane (so the core is busy folding a backlog when
/// the start fails) and then refuses its first subscription, as a gatewayhub's refusal would.
struct FloodThenRefuse {
    refusal: String,
}

struct FloodingFeed {
    refusal: String,
    sink: Arc<dyn LiveDataSink>,
}

impl DataClient for FloodingFeed {
    fn subscribe_bars(
        &mut self,
        symbol: &str,
        _interval: &str,
    ) -> Result<vike_data::SubscriptionId, vike_data::LiveDataError> {
        for i in 0..100_000_i64 {
            self.sink.quote(
                "deribit",
                symbol,
                vike_marketdata::QuoteTick {
                    ts: i,
                    local_ts: i,
                    bid: 100.0,
                    ask: 100.5,
                    bid_size: 1.0,
                    ask_size: 1.0,
                    symbol: symbol.to_string(),
                },
            );
        }
        Err(vike_data::LiveDataError::Subscribe(self.refusal.clone()))
    }
    fn subscribe_quotes(
        &mut self,
        _symbol: &str,
    ) -> Result<vike_data::SubscriptionId, vike_data::LiveDataError> {
        Err(vike_data::LiveDataError::Subscribe(self.refusal.clone()))
    }
    fn subscribe_trades(
        &mut self,
        _symbol: &str,
    ) -> Result<vike_data::SubscriptionId, vike_data::LiveDataError> {
        Err(vike_data::LiveDataError::Subscribe(self.refusal.clone()))
    }
    fn subscribe_book(
        &mut self,
        _symbol: &str,
    ) -> Result<vike_data::SubscriptionId, vike_data::LiveDataError> {
        Err(vike_data::LiveDataError::Subscribe(self.refusal.clone()))
    }
    fn unsubscribe(&mut self, _id: vike_data::SubscriptionId) {}
    fn shutdown(&mut self) {}
}

impl FeedCtors for FloodThenRefuse {
    fn strat(
        &self,
        venue: &str,
        _network: Option<&str>,
        _sink: Arc<dyn LiveDataSink>,
    ) -> Result<vike_datahub_client::strat::StratFeed, String> {
        Err(format!("{venue}: no datahub in this test"))
    }

    fn client(
        &self,
        _venue: &str,
        _network: Option<&str>,
        sink: Arc<dyn LiveDataSink>,
    ) -> Result<Box<dyn DataClient + Send>, String> {
        Ok(Box::new(FloodingFeed { refusal: self.refusal.clone(), sink }))
    }
}

/// **A feed that REFUSES after the core is up ends the start the way the lease refusal does**: the feeds
/// already wired are released and the core is told to stop and JOINED, so the process does not exit under a
/// core thread still folding (`CoreHandle` has no `Drop`: dropping it closes the lane, and the core's own
/// teardown, the final snapshot and the cancel sweep, then runs while `main` returns). The refusal's words
/// still reach the operator.
///
/// The feed floods the core's tick lane before it refuses, so a core that is NOT joined is still folding
/// its backlog when the start returns: the check (no wait) is that no `vt-core` thread this start created is
/// alive. It reads `/proc` (Linux, where tests run) and names the threads THIS start created, so a core
/// another test runs in the same process does not count; the suite runs under `nextest` in any case.
#[cfg(target_os = "linux")]
#[test]
fn a_feed_that_refuses_after_the_core_is_up_tears_the_core_down() {
    let before = live_core_threads();
    let lock_dir = tempfile::tempdir().expect("a throwaway state dir");
    let refusal = "deribit subscribe_bars: the gatewayhub refused the stream (a test's)";
    let err = super::seam_live_runtime_over(
        vec![super::buy_hold_mount("deribit", false)],
        None,
        &super::SeamPolicy::armed(&[("deribit", vike_model::AccountTier::Paper)]),
        HashMap::new(),
        lock_dir.path(),
        &FloodThenRefuse { refusal: refusal.to_string() },
        &LocalCtor,
    )
    .err()
    .expect("a refused feed refuses the start");
    assert!(err.contains(refusal), "the refusal's words survive: {err}");
    let leaked: Vec<String> = live_core_threads().difference(&before).cloned().collect();
    assert!(
        leaked.is_empty(),
        "the refused start returned with its core thread still running (tids {leaked:?}): \
         `CoreHandle` has no `Drop`, so the start must stop and join it"
    );
}

/// One polymarket mount of token `TOK` at `interval`, through the profile load path.
fn poly_mount(interval: &str, interval_ms: u64) -> ResolvedAdapter {
    let toml = format!(
        "venue = \"polymarket\"\ntoken_id = \"TOK\"\nqty = 20.0\nresolution_ts_ms = 1793491200000\n\
         interval = \"{interval}\"\ninterval_ms = {interval_ms}\n\n[strategy]\nname = \"buy_hold\"\n\n\
         [strategy.params]\nsize = 1.0\n"
    );
    let row = crate::config::DaemonProfile::from_toml_str(&toml)
        .unwrap_or_else(|e| panic!("the polymarket profile parses: {e:?}"));
    let cfg = row.to_maker_config();
    let spec = row.to_mount_spec();
    let strategy = row.resolve_strategy(&cfg).expect("buy_hold resolves from the registry");
    ResolvedAdapter { row, cfg, spec, strategy }
}

/// **Two polymarket rows on ONE token at different intervals are refused before any core spawns** (they
/// share one feed and one bar synthesizer window, so the second interval would silently never fire), and
/// two on different tokens, or at one interval, are not. This is the gate `live_runtime_with` calls
/// beside `check_ctrader_intervals`; it had no test of its own.
#[test]
fn two_polymarket_rows_on_one_token_at_different_intervals_are_refused_by_row() {
    let err = crate::feeds::check_poly_token_intervals(&[
        poly_mount("1m", 60_000),
        poly_mount("5m", 300_000),
    ])
    .expect_err("one token, two windows");
    assert!(
        err.contains("mounts[0] and mounts[1]") && err.contains("different intervals"),
        "{err}"
    );
    assert!(
        crate::feeds::check_poly_token_intervals(&[
            poly_mount("1m", 60_000),
            poly_mount("1m", 60_000),
        ])
        .is_ok(),
        "one token at ONE interval is one window"
    );
}

// ---- W12c: the transient refusal after the core, the key minted later, the banner ---------------------------------

/// A constructor whose datahub PROBE passes (the datahub is up) and whose every feed is refused as the engine
/// refuses a link that is down: the datahub's own link to the gatewayhub is away, which the probe cannot see.
struct UpButItsGatewayIsDown;

impl FeedCtors for UpButItsGatewayIsDown {
    fn strat(
        &self,
        venue: &str,
        _network: Option<&str>,
        _sink: Arc<dyn LiveDataSink>,
    ) -> Result<vike_datahub_client::strat::StratFeed, String> {
        Err(format!("{venue}: this scripted constructor builds client() feeds only"))
    }

    fn client(
        &self,
        _venue: &str,
        _network: Option<&str>,
        sink: Arc<dyn LiveDataSink>,
    ) -> Result<Box<dyn DataClient + Send>, String> {
        let _ = sink;
        Ok(Box::new(RefusingFeed(TRANSIENT_REFUSAL)))
    }

    fn datahub_probe(&self, _venues: &[String]) -> Option<DatahubProbe> {
        Some(DatahubProbe::new(|| None))
    }
}

/// A feed whose every subscribe is refused with the text it was built with.
struct RefusingFeed(&'static str);

impl DataClient for RefusingFeed {
    fn subscribe_bars(
        &mut self,
        _symbol: &str,
        _interval: &str,
    ) -> Result<vike_data::SubscriptionId, vike_data::LiveDataError> {
        Err(vike_data::LiveDataError::Subscribe(self.0.to_string()))
    }
    fn subscribe_quotes(
        &mut self,
        _symbol: &str,
    ) -> Result<vike_data::SubscriptionId, vike_data::LiveDataError> {
        Err(vike_data::LiveDataError::Subscribe(self.0.to_string()))
    }
    fn subscribe_trades(
        &mut self,
        _symbol: &str,
    ) -> Result<vike_data::SubscriptionId, vike_data::LiveDataError> {
        Err(vike_data::LiveDataError::Subscribe(self.0.to_string()))
    }
    fn subscribe_book(
        &mut self,
        _symbol: &str,
    ) -> Result<vike_data::SubscriptionId, vike_data::LiveDataError> {
        Err(vike_data::LiveDataError::Subscribe(self.0.to_string()))
    }
    fn unsubscribe(&mut self, _id: vike_data::SubscriptionId) {}
    fn shutdown(&mut self) {}
}

/// What `RemoteDataClient::subscribe` says for a link that is down (its text, with the transient mark).
const TRANSIENT_REFUSAL: &str = "gatewayhub 127.0.0.1:1: the link is down (not dialled yet); the subscription \
     was not started and is retried on the caller's next pass";

/// **A transient subscribe refusal AFTER the core is up, where no live account of the venue runs, is the same
/// held feed as the probe's verdict, not `ExitCode::FAILURE`** (review 17, finding 1): the datahub passed the
/// probe while its own link to the gatewayhub is away, the first subscribe is refused as a link that is down, and
/// the start survives: the venue's feed is unwired, the `paper` account runs without prices, the late arm waits
/// for the datahub AND the gatewayhub, and the ready banner says `PRICE FEEDS UNAVAILABLE`. The control is the
/// refusal that is NOT transient (`a_feed_that_refuses_after_the_core_is_up_tears_the_core_down`): it still ends
/// the start.
#[test]
fn a_transient_refusal_after_the_core_is_survived_where_no_live_account_runs() {
    let lock_dir = tempfile::tempdir().expect("a throwaway state dir");
    let (handle, teardown, _live, late_arm, _nr) = super::seam_live_runtime_over(
        vec![super::buy_hold_mount("deribit", false)],
        None,
        &super::SeamPolicy::armed(&[("deribit", vike_model::AccountTier::Paper)]),
        HashMap::new(),
        lock_dir.path(),
        &UpButItsGatewayIsDown,
        &WaitingCtor,
    )
    .expect("a transient refusal with no live account running is not a refused start");
    assert!(teardown.feeds.is_empty(), "the refused venue's feed is not wired");
    let plan = late_arm.expect("the feeds wait: the late arm has a plan");
    assert!(plan.datahub.is_some(), "…for the datahub");
    // …and NOT for the gatewayhub: this box has only a `paper` account, which arms through nothing, and may
    // hold no gatewayhub key at all (`gateway_trade_link` hands the link an empty key then), so a wait on its
    // own link would never end. The doc of `LateArmPlan::gateway` says exactly this for a datahub-only wait.
    assert!(!plan.gateway, "a paper-only box must not wait for a gatewayhub link: {plan:?}");
    assert!(
        plan.answers_words().contains("datahub") && !plan.answers_words().contains("gatewayhub")
    );
    // The behavioural half: the datahub's probe passes and the tradehub's own link (a dead address, an empty
    // key) never will; the late arm can still fire, because it asks the datahub alone.
    assert!(plan.answers(), "a datahub-only wait is over when the datahub answers");
    let why = plan.feeds_why.as_deref().expect("the reason travels to the banner");
    assert!(why.contains("the link is down"), "{why}");
    let mode = crate::tradehub_cli::banner::feeds_unavailable_mode("PAPER".to_string(), Some(why));
    assert!(
        mode.starts_with("PRICE FEEDS UNAVAILABLE: ") && mode.ends_with(" — PAPER"),
        "the banner leads with the outage and keeps the mode grammar: {mode}"
    );
    super::tear_down(handle, teardown);
}

// ---- W12e: a CAP refusal needs an operator, so it is survived without a restart (or ends in a status the unit keeps) ---

/// What the gatewayhub's pump cap says when it is full of running pumps (its text, with `CAP_MARK`).
const CAP_REFUSAL: &str = "deribit subscribe_bars BTC-PERPETUAL@1m: Subscribe(\"gatewayhub: already hosting 16 tick \
     pumps (the daemon's cap; 0 of them being stopped); a cap, not a link: raise the cap or drop a subscription\")";

/// A constructor whose datahub PROBE passes and whose every feed is refused by a full cap.
struct UpButCapped;

impl FeedCtors for UpButCapped {
    fn strat(
        &self,
        _venue: &str,
        _network: Option<&str>,
        _sink: Arc<dyn LiveDataSink>,
    ) -> Result<vike_datahub_client::strat::StratFeed, String> {
        Err(CAP_REFUSAL.to_string())
    }

    fn client(
        &self,
        _venue: &str,
        _network: Option<&str>,
        _sink: Arc<dyn LiveDataSink>,
    ) -> Result<Box<dyn DataClient + Send>, String> {
        Ok(Box::new(RefusingFeed(CAP_REFUSAL)))
    }

    fn datahub_probe(&self, _venues: &[String]) -> Option<DatahubProbe> {
        Some(DatahubProbe::new(|| None))
    }
}

/// **A CAP refusal where no live account runs keeps the process UP and priceless, and orders NO restart.** A restart
/// meets the same full cap, so the transient class's answer (unwire the feed, wait, restart armed) would loop the
/// whole process, and a supervisor loop on a plain failure exit would be worse. The venue's feed is unwired, the
/// `paper` account runs, the banner names the cap and what to change, ONE `error` line says so, the plan carried
/// out is a notice with nothing to wait for (no probe, no thread, `spawn` answers `None`), and the restart ledger
/// is never written.
#[test]
fn a_cap_refusal_keeps_the_process_up_priceless_and_orders_no_restart() {
    let dir = tempfile::tempdir().expect("a throwaway state dir");
    let (started, events) = vike_log::capture::captured(|| {
        super::seam_live_runtime_over(
            vec![super::buy_hold_mount("deribit", false)],
            None,
            &super::SeamPolicy::armed(&[("deribit", vike_model::AccountTier::Paper)]),
            HashMap::new(),
            dir.path(),
            &UpButCapped,
            &WaitingCtor,
        )
    });
    let (handle, teardown, _live, late_arm, _nr) =
        started.expect("a cap refusal with no live account running is not a refused start");
    assert!(teardown.feeds.is_empty(), "the capped venue's feed is not wired");
    let plan = late_arm.expect("a notice carries the cause to the banner");
    assert!(
        plan.waiting.is_empty() && plan.datahub.is_none() && !plan.gateway,
        "a cap waits for NOTHING: {plan:?}"
    );
    let aged = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(120))
        .expect("a clock");
    assert!(
        crate::late_arm::spawn(plan.clone(), aged, Arc::default()).is_none(),
        "no late-arm thread exists for a cap, so no restart can ever be ordered"
    );
    assert!(
        !dir.path().join(crate::late_arm::RESTART_LEDGER_FILE).exists(),
        "nothing counted, nothing restarted"
    );
    let why = plan.feeds_why.as_deref().expect("the cause travels to the banner");
    assert!(
        why.starts_with(crate::feeds::FEEDS_CAPPED_MARK)
            && why.contains("already hosting 16 tick pumps"),
        "{why}"
    );
    let mode = crate::tradehub_cli::banner::feeds_unavailable_mode("PAPER".to_string(), Some(why));
    assert!(
        mode.starts_with("PRICE FEEDS UNAVAILABLE: a cap on the price path is full (")
            && mode.contains("already hosting 16 tick pumps")
            && mode.contains("raise that cap or drop a subscription")
            && mode.contains("no restart is ordered")
            && mode.ends_with(" — PAPER"),
        "the banner names the cap and what the operator changes: {mode}"
    );
    let said = events
        .iter()
        .filter(|e| {
            e.level == tracing::Level::ERROR && e.message.contains("PRICE FEEDS UNAVAILABLE (CAP)")
        })
        .count();
    assert_eq!(said, 1, "the cap is said at error once per start: {events:?}");
    super::tear_down(handle, teardown);
}

/// The scripted backend whose default `binance` demo account STARTS (a recording exec client at the demo tier), so the
/// account RUNS and is in `live_venues`: the venue the cap then refuses has a running live account.
struct RunningDemoCtor;

impl BackendCtor for RunningDemoCtor {
    fn backend(
        &self,
        _vars: &HashMap<String, String>,
        policy: &vike_tradehub_core::AdapterPolicy,
        _data_only: &HashSet<String>,
    ) -> Arc<dyn vike_tradehub_core::VenueBackend> {
        Arc::new(
            vike_tradehub_core::ScriptedBackend::credentialed(
                policy,
                &crate::scripted_arming_tests::absent_venues(),
            )
            .script_start(
                "binance",
                Ok(vike_tradehub_core::AdapterBind {
                    client: Box::new(vike_exec::testing::RecordingClient::default()),
                    recon: None,
                    bound_tier: vike_model::AccountTier::Demo,
                    grid: None,
                    contract_value: None,
                    margin_mode: None,
                    leg_grids: Vec::new(),
                    confirmed_book: None,
                }),
            ),
        )
    }
}
/// **A cap refusal of a venue with a RUNNING live account cannot be held (the core cannot drop an engine), and ends
/// the start with the status the unit does not restart on**, not the plain failure a `RestartSec=5` loop would
/// repeat for ever over the other venues' engines. The control: any other refusal keeps the plain failure.
#[test]
fn a_cap_refusal_of_a_venue_with_a_running_live_account_ends_the_start_with_the_status_the_unit_does_not_restart_on()
 {
    let dir = tempfile::tempdir().expect("a throwaway state dir");
    let err = super::seam_live_runtime_over(
        binance_mount(),
        Some(super::oanda::fill_sized_budget()),
        &{
            let mut book = vike_secrets::account_credentials::CredentialBook::new();
            book.insert_account(1, "API_KEY", "fake-binance-key");
            book.insert_account(1, "API_SECRET", "fake-binance-secret");
            super::SeamPolicy::armed(&[("binance", vike_model::AccountTier::Demo)]).holding(book)
        },
        HashMap::new(),
        dir.path(),
        &UpButCapped,
        &RunningDemoCtor,
    )
    .err()
    .expect("a running demo account cannot be left without a feed: the start ends");
    assert!(err.contains("already hosting 16 tick pumps"), "the cap's words survive: {err}");
    assert_eq!(
        crate::feed_gate::failed_start_code(&err),
        Some(crate::late_arm::EXIT_OPERATOR_NEEDED),
        "{err}"
    );
    assert_eq!(
        crate::feed_gate::failed_start_code(
            "deribit subscribe_bars: Subscribe(\"no such interval\")"
        ),
        None,
        "every other refusal keeps the plain failure"
    );
    assert!(!dir.path().join(crate::late_arm::RESTART_LEDGER_FILE).exists());
}

/// The survived-transient start of[`a_transient_refusal_after_the_core_is_survived_where_no_live_account_runs`],
/// with `state_dir` as the project's state directory (where the restart ledger lives).
fn survived_transient_start(state_dir: &std::path::Path) -> Option<LateArmPlan> {
    let (handle, teardown, _live, late_arm, _nr) = super::seam_live_runtime_over(
        vec![super::buy_hold_mount("deribit", false)],
        None,
        &super::SeamPolicy::armed(&[("deribit", vike_model::AccountTier::Paper)]),
        HashMap::new(),
        state_dir,
        &UpButItsGatewayIsDown,
        &WaitingCtor,
    )
    .expect("a transient refusal with no live account running is not a refused start");
    super::tear_down(handle, teardown);
    late_arm
}

/// **A persistent transient does not restart the process for ever** (review 20, finding 3). The cause the late
/// arm cannot see (the datahub's own link to the gatewayhub, broken for good) passes the late arm's checks at the
/// 90 s mark, so every restart meets the same refusal and the process restarts again, every ~95 s. The feed-driven
/// restarts are counted in the project's state directory: this plays process starts one after another, each one
/// "restarting" (a restart is the ledger's count) exactly as `late_arm::spawn` does, and the fourth start in the
/// hour stays up without prices, with no restart waiting and a banner that says the budget is spent. The
/// mendable cause (the first starts) still gets its restarts.
#[test]
fn a_persistent_transient_restarts_the_process_a_bounded_number_of_times_then_stays_up_priceless() {
    use crate::late_arm::{FEED_RESTARTS_PER_WINDOW, RESTARTS_SPENT_MARK, RestartLedger};
    let dir = tempfile::tempdir().expect("a throwaway state dir");
    let mut restarts = 0;
    let mut last = None;
    for start in 0..FEED_RESTARTS_PER_WINDOW + 3 {
        let plan =
            survived_transient_start(dir.path()).expect("the feeds wait, or say they do not");
        if plan.datahub.is_some() {
            // The late arm fires (its checks pass): it counts the restart before ordering it.
            assert!(plan.waiting.contains(&crate::late_arm::FEEDS_WAIT_ENTRY.to_string()));
            plan.restarts.record(vike_model::now_ms()).expect("the ledger is writable");
            restarts += 1;
        } else {
            last = Some((start, plan));
            break;
        }
    }
    assert_eq!(restarts, FEED_RESTARTS_PER_WINDOW, "the budget bounds the restarts");
    let (start, plan) = last.expect("a start with the budget spent");
    assert_eq!(start, FEED_RESTARTS_PER_WINDOW);
    assert!(plan.waiting.is_empty(), "nothing waits for a restart any more: {:?}", plan.waiting);
    assert!(plan.datahub.is_none(), "no probe is kept: nothing would ever fire");
    assert!(
        crate::late_arm::spawn(plan.clone(), std::time::Instant::now(), Arc::default()).is_none()
    );
    let why = plan.feeds_why.as_deref().expect("the banner still says there are no prices");
    assert!(why.starts_with(RESTARTS_SPENT_MARK) && why.contains("the link is down"), "{why}");
    assert_eq!(
        crate::feeds::short_cause(why),
        "the late-arm restart budget is spent: the price path keeps refusing (restart by hand once it is fixed)"
    );
    let mode = crate::tradehub_cli::banner::feeds_unavailable_mode("PAPER".to_string(), Some(why));
    assert!(
        mode.starts_with("PRICE FEEDS UNAVAILABLE: the late-arm restart budget is spent"),
        "{mode}"
    );
    // The counter lives in the state directory, across process starts.
    assert_eq!(
        RestartLedger::in_dir(dir.path()).recent(vike_model::now_ms()),
        Ok(FEED_RESTARTS_PER_WINDOW)
    );
}

/// **The banner says plainly what the missing key is**, in the words the error line has: a start with no
/// strategy-lane key is up, prints the ready line, and nothing else tells the operator it has no prices.
#[test]
fn the_banner_names_a_missing_strategy_lane_key_in_the_error_lines_words() {
    let ctors = crate::feeds::ProdFeedCtors::new("127.0.0.1:1", None);
    let why = ctors.datahub_probe(&[]).expect("production asks").check().expect("no key");
    let mode = crate::tradehub_cli::banner::feeds_unavailable_mode(
        "LIVE (venue=none)".to_string(),
        Some(&why),
    );
    assert_eq!(mode, "PRICE FEEDS UNAVAILABLE: no strategy-lane key — LIVE (venue=none)");
    assert_eq!(
        crate::tradehub_cli::banner::feeds_unavailable_mode("PAPER".to_string(), None),
        "PAPER",
        "nothing waits: the banner is byte-identical"
    );
}

/// **A key minted AFTER the start is found by the next probe** (the late arm polls it): the key store is read
/// again at every ask, so a start that found none no longer waits for ever on an answer that cannot change. The
/// datahub here is nothing at all, so the answer moves from "holds no strategy-lane key" to a DIAL failure the
/// moment the key exists: the key was read.
#[test]
fn a_key_minted_between_polls_is_found_by_the_next_probe() {
    let minted: Arc<std::sync::Mutex<Option<Vec<u8>>>> = Arc::new(std::sync::Mutex::new(None));
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        l.local_addr().expect("addr").to_string()
    };
    let source = {
        let minted = Arc::clone(&minted);
        move || minted.lock().expect("key").clone()
    };
    let ctors = crate::feeds::ProdFeedCtors::with_key_source(dead.clone(), source);
    let probe = ctors.datahub_probe(&[]).expect("production asks");

    let first = probe.check().expect("no key yet");
    assert!(first.contains("holds no strategy-lane key"), "{first}");
    assert_eq!(crate::feeds::short_cause(&first), "no strategy-lane key");

    *minted.lock().expect("key") = Some(b"a-trade-key".to_vec());
    let second = probe.check().expect("the datahub is not there");
    assert!(
        !second.contains("holds no strategy-lane key") && second.contains(&dead),
        "the second poll read the minted key and dialled: {second}"
    );
    assert_eq!(
        crate::feeds::short_cause(&second),
        "the price path (datahub or gatewayhub) does not answer"
    );
}
