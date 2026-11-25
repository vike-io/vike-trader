//! **A tradehub whose EVERY account fails at its adapter start STAYS UP with no engine, and the account joins and
//! trades when its venue connects, with the book the boot would have restored for it** (owner 2026-11-15, "1.a"),
//! proved against the REAL binary: the live gate on, one deribit `demo` account (its feed rides the datahub's strategy
//! lane alone, no tick pump) with `buy_hold.rhai` mounted on it, a gatewayhub (the gatewayhub client's own fake peer)
//! that arms the account and answers its adapter start `venue unreachable` (an IB Gateway that is down) while the test
//! says the venue is down, and a real datahub strategy lane over a scripted venue client the test pushes prices through.
//!
//! Before the change that start ended in `NoAccountRuns` and exited for its supervisor to restart every 5 s. Now the
//! node starts with no engine (`vike_core::spawn_core_without_accounts`), says why (`venue deribit not connected`), prints
//! its ready line, stays up, and the account watcher asks the account again on its backoff: once the venue answers,
//! the account joins the running core with its deactivated mount. A position the account held before the restart comes
//! back as the position it was (the journal's book, not a reconcile divergence), and its strategy does not buy again.
//! A start the venue REFUSES (a declined key, `adapter failed`) parks: the node stays up and never asks again by itself.
//!
//! Unix only: the stop is a POSIX signal (`sigterm_stop.rs`'s reason).

#![cfg(unix)]

use crate::daemon_profile_seed;
use crate::temp_project::temp_dir;
use crate::venue_feed_splice_smoke::{AcknowledgingClient, SMOKE_TRADE_KEY, scripted_datahub_with};

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_data::DataClientSink;
use vike_gatewayhub_client::proto::{
    AdapterReply, ArmingRowWire, ArmingTableWire, Request, Response, TradeDown, TradeUp,
    trade_venue_feature,
};
use vike_gatewayhub_client::test_support::fake_trade_peer::{
    Step, TradePeer, TradePeerConfig, default_script,
};
use vike_hub_protocol::features::market_data_venue_feature;
use vike_log::reaper::Reaper;
use vike_model::events::{Event, FillEvent, OrderAccepted, OrderFilled, PositionSide, TradeId};
use vike_model::{AccountTier, ArmingBlock, OrderRequest};

/// How long each awaited fact may take on a loaded box.
const PATIENCE: Duration = Duration::from_secs(90);

/// The venue, its account's symbol and the price the test quotes it at.
const VENUE: &str = "deribit";
const SYMBOL: &str = "BTC-PERPETUAL";
const PRICE: f64 = 50_000.0;

/// The gatewayhub's arming table: deribit's default account, armed at `demo`.
fn arming() -> ArmingTableWire {
    ArmingTableWire {
        properties: std::collections::BTreeMap::from([(
            VENUE.to_string(),
            vike_model::VenueAdapterDeclarationView {
                adapter_present: true,
                addresses_accounts: true,
                takes_reconcile_trigger: false,
                grid_source: vike_model::GridSource::NoGrid,
                process_exclusive: None,
                clock_declaration_word: vike_model::ClockDeclarationKind::Wired,
            },
        )]),
        rows: vec![ArmingRowWire {
            venue: VENUE.to_string(),
            label: None,
            tier: Some(AccountTier::Demo),
            effective: Some(AccountTier::Demo),
            block_word: ArmingBlock::None.word().to_string(),
            account_ids: vec![1],
            book: None,
        }],
        key_changed: Vec::new(),
        start_inputs: Default::default(),
    }
}

/// The adapter's answer: the key declined (a refusal) while the venue declines it, else up at `demo` once the venue
/// is, else the venue unreachable (transient).
fn adapter_reply(up: bool, declined: bool) -> AdapterReply {
    if declined {
        return AdapterReply::NotRunning {
            block_word: ArmingBlock::AdapterFailed.word().to_string(),
            refusal_text: "the venue declined the key (test)".to_string(),
        };
    }
    if !up {
        return AdapterReply::NotRunning {
            block_word: ArmingBlock::VenueUnreachable.word().to_string(),
            refusal_text: "the venue's gateway is not running (test)".to_string(),
        };
    }
    AdapterReply::Running {
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
        broker_code_len: 0,
    }
}

/// The fake gatewayhub and the scripted datahub one test drives, and what they saw.
struct Hubs {
    peer: TradePeer,
    venue_up: Arc<AtomicBool>,
    /// The venue declines the account's key: its adapter start answers `adapter failed`.
    declined: Arc<AtomicBool>,
    /// How many adapter starts the gatewayhub was asked for.
    starts: Arc<AtomicUsize>,
    orders: Arc<Mutex<Vec<OrderRequest>>>,
    datahub: String,
    sink: Arc<Mutex<Option<Arc<dyn DataClientSink>>>>,
}

impl Hubs {
    fn start(venue_up: bool) -> Self {
        let up_flag = Arc::new(AtomicBool::new(venue_up));
        let declined: Arc<AtomicBool> = Arc::default();
        let starts: Arc<AtomicUsize> = Arc::default();
        let orders: Arc<Mutex<Vec<OrderRequest>>> = Arc::default();
        let peer = {
            let up = Arc::clone(&up_flag);
            let refused = Arc::clone(&declined);
            let asked = Arc::clone(&starts);
            let seen = Arc::clone(&orders);
            let healthy = default_script();
            let mut config = TradePeerConfig::new()
                .with_rpc(|request| match request {
                    Request::ArmingQuery => vec![Response::ArmingTable(Box::new(arming()))],
                    Request::PreflightQuery { .. } => {
                        vec![Response::PreflightLegs { legs: Vec::new(), plan: None }]
                    }
                    _ => vec![Response::Error("the test's gatewayhub: no script".to_string())],
                })
                .with_script(move |frame| match frame {
                    TradeUp::AdapterStart(start) => {
                        asked.fetch_add(1, Ordering::SeqCst);
                        vec![Step::Send(TradeDown::AdapterReply {
                            rid: start.rid,
                            reply: Box::new(adapter_reply(
                                up.load(Ordering::SeqCst),
                                refused.load(Ordering::SeqCst),
                            )),
                        })]
                    }
                    other => healthy(other),
                })
                .with_command_observer(move |frame| {
                    if let TradeUp::Submit { order, .. } = frame {
                        seen.lock().expect("orders").push(order.clone());
                    }
                });
            config.features.push(market_data_venue_feature(VENUE));
            config.features.push(trade_venue_feature(VENUE));
            config.route_key = VENUE.to_string();
            TradePeer::start(config)
        };
        let sink: Arc<Mutex<Option<Arc<dyn DataClientSink>>>> = Arc::default();
        let datahub = {
            let kept = Arc::clone(&sink);
            scripted_datahub_with(
                Box::new(move |_venue, given| {
                    *kept.lock().expect("sink") = Some(given);
                    Ok(Box::new(AcknowledgingClient(0)))
                }),
                &[VENUE],
            )
        };
        Hubs { peer, venue_up: up_flag, declined, starts, orders, datahub, sink }
    }

    fn orders(&self) -> Vec<OrderRequest> {
        self.orders.lock().expect("orders").clone()
    }

    /// One mark and one closed 1m bar of the venue's symbol (the strategy's `on_bar`), through the datahub.
    fn push_prices(&self) {
        if let Some(sink) = self.sink.lock().expect("sink").clone() {
            let now = vike_model::now_ms();
            sink.mark_tick(VENUE, SYMBOL, PRICE, now);
            let bar = vike_marketdata::Bar::ohlcv(
                (now / 60_000 - 1) * 60_000,
                PRICE,
                PRICE,
                PRICE,
                PRICE,
                1.0,
            );
            sink.close_bar(VENUE, SYMBOL, "1m", bar);
        }
    }

    /// The venue fills `order` whole at [`PRICE`]: its acknowledgement, the fill and the terminal, on the lease.
    fn fill(&self, order: &OrderRequest) {
        let fill = FillEvent {
            trade_id: TradeId::new("t-core-no-engine-1").expect("a trade id"),
            order_client_id: order.order_client_id.clone(),
            venue: VENUE.into(),
            symbol: order.symbol.as_str().into(),
            side: order.side,
            last_qty: order.qty,
            last_price: PRICE,
            commission: 0.0,
            commission_asset: String::new().into(),
            liquidity_side: "taker".to_string().into(),
            timestamp: vike_model::now_ms(),
            mark_price: Some(PRICE),
            position_side: PositionSide::Both,
        };
        self.peer.push_event(Event::OrderAccepted(OrderAccepted {
            order_client_id: order.order_client_id.clone(),
            venue_order_id: Some(format!("v-{}", order.order_client_id).into()),
            timestamp: vike_model::now_ms(),
        }));
        self.peer.push_event(Event::Fill(fill.clone()));
        self.peer.push_event(Event::OrderFilled(OrderFilled {
            order_client_id: order.order_client_id.clone(),
            fill,
            timestamp: vike_model::now_ms(),
        }));
    }
}

/// A project whose one account is deribit's default `demo` account, mounted with `buy_hold.rhai`, dialling `hubs`.
fn project(dir: &Path, hubs: &Hubs) {
    let settings = dir.join("settings");
    vike_secrets::plant_settings_rows(
        &settings,
        &vike_secrets::StoredSettings {
            settings: [
                ("flags", "tradehub_live", "true".to_string()),
                ("flags", "preflight_skip", "true".to_string()),
                ("config", "gatewayhub_addr", format!("\"{}\"", hubs.peer.address_string())),
                ("config", "datahub_addr", format!("\"{}\"", hubs.datahub)),
                // The journal is on only with this row: the restart restores the book from it.
                (
                    "config",
                    "journal_dir",
                    serde_json::to_string(&dir.join("journal").display().to_string())
                        .expect("a path as JSON"),
                ),
            ]
            .into_iter()
            .map(|(section, key, value)| vike_secrets::SettingRow {
                section: section.to_string(),
                key: key.to_string(),
                value,
            })
            .collect(),
            ..Default::default()
        },
    )
    .expect("plant the rows");
    daemon_profile_seed::seed_active_daemon_profile(
        &settings,
        "core-no-engine",
        &format!(
            "venue = \"{VENUE}\"\n\
             asset_class = \"CryptoPerp\"\n\
             symbol = \"{SYMBOL}\"\n\
             interval = \"1m\"\n\
             interval_ms = 60000\n\
             tick_size = 0.5\n\
             \n\
             [strategy]\n\
             file = \"buy_hold.rhai\"\n\
             \n\
             [strategy.params]\n\
             size = 0.01\n\
             \n\
             [daemon]\n\
             summary_ms = 1000\n\
             shutdown_deadline_ms = 5000\n"
        ),
    );
    seed_run_part(&settings, "core-no-engine");
    // The project token the fake gatewayhub states, where a gatewayhub of this project leaves it.
    let state = settings.join("state");
    std::fs::create_dir_all(&state).expect("create the state dir");
    std::fs::write(
        state.join(vike_paths::state_path::PROJECT_TOKEN_FILE),
        "project-token-of-the-tests",
    )
    .expect("the token");
    let created = vike_secrets::edit_account_in_unjournalled(
        &settings,
        vike_secrets::AccountEdit::Create { venue: VENUE, tier: AccountTier::Demo, label: None },
    )
    .expect("a demo row");
    let id = created.after.expect("a created row").id;
    vike_secrets::edit_account_in_unjournalled(
        &settings,
        vike_secrets::AccountEdit::SetSymbol { id, symbol: Some(SYMBOL) },
    )
    .expect("state the symbol");
    // The Trade key the fake gatewayhub verifies.
    let keys = vike_gatewayhub_client::test_support::fake_trade_peer::trade_keys();
    let trade_key = keys.key_for(vike_hub_protocol::auth::Scope::Trade).to_vec();
    vike_secrets::save_credentials_to_store_unjournalled(
        &settings,
        vike_secrets::StoreTable::HubKey,
        &[
            (
                vike_venues::credential_keys::gatewayhub_trade_key().to_string(),
                String::from_utf8(trade_key).expect("the test key is text"),
            ),
            (
                vike_hub_protocol::auth::DATAHUB_TRADE_KEY_ENV.to_string(),
                SMOKE_TRADE_KEY.to_string(),
            ),
        ],
        vike_secrets::CredentialUpsertIntent::Add,
    )
    .expect("file the hub keys");
}

/// The run part a `demo` account needs (its `[risk]` budget: no budget, no venue-client engine),
/// written into the active daemon body `daemon` (decision 0297: one live profile per daemon).
fn seed_run_part(settings: &Path, daemon: &str) {
    use vike_secrets::profile_store::{ProfileOperatorWrite, read_profiles, store_profile};
    let db = vike_secrets::db_path_in(settings);
    let mut record = read_profiles(&db)
        .expect("read the profile rows")
        .by_name(daemon)
        .expect("the daemon body is stored")
        .clone();
    record.settings.extend(
        [("risk.max_notional_per_order", "1000000.0"), ("risk.max_total_exposure", "1000000.0")]
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.to_string())),
    );
    let write = ProfileOperatorWrite::claim("vike-tradehub test seed");
    store_profile(&db, &record, &write, 0, vike_model::AssetClass::SQL_WORDS)
        .unwrap_or_else(|e| panic!("store the run part: {e}"));
}

/// The shipped tradehub, started in `dir`, and what it said (`paper_only_prices.rs` drives it too).
pub(crate) struct Node {
    child: Reaper,
    lines: Receiver<String>,
    stderr: Arc<Mutex<String>>,
}

impl Node {
    pub(crate) fn start(dir: &Path) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_vike-tradehub"));
        command
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("VIKE_SETTINGS_DIR", dir.join("settings"))
            .env_remove("VIKE_STATE_ROOT")
            .env("VIKE_LOG_DIR", dir.join("logs"))
            .env_remove("VIKE_LOG_FILE_LEVEL")
            .env_remove("RUST_LOG")
            .env_remove("VIKE_LOG");
        for r in vike_config::REMOVED_ENV {
            command.env_remove(r.var);
        }
        let mut child = Reaper(command.spawn().expect("spawn vike-tradehub"));
        let (tx, lines) = mpsc::channel();
        let stdout = child.0.stdout.take().expect("piped stdout");
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr = Arc::new(Mutex::new(String::new()));
        let buffer = Arc::clone(&stderr);
        let err_pipe = child.0.stderr.take().expect("piped stderr");
        std::thread::spawn(move || {
            for line in BufReader::new(err_pipe).lines().map_while(Result::ok) {
                let mut text = buffer.lock().expect("stderr buffer");
                text.push_str(&line);
                text.push('\n');
            }
        });
        Node { child, lines, stderr }
    }

    pub(crate) fn log(&self) -> String {
        self.stderr.lock().expect("stderr buffer").clone()
    }

    pub(crate) fn wait_log(&self, needle: &str) {
        let found = vike_log::poll::wait_until(PATIENCE, || self.log().contains(needle));
        assert!(found, "no `{needle}` within {PATIENCE:?}; stderr:\n{}", self.log());
    }

    /// The next stdout protocol line `hit` accepts, failing on a dead node (the restart loop this file guards against).
    pub(crate) fn wait_line(&mut self, what: &str, mut hit: impl FnMut(&str) -> bool) -> String {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left.max(Duration::from_millis(1))) {
                Ok(line) if hit(&line) => return line,
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => {
                    panic!("no {what} within {PATIENCE:?}; stderr:\n{}", self.log())
                }
                Err(RecvTimeoutError::Disconnected) => {
                    let status = self.child.0.wait().expect("wait");
                    panic!(
                        "the tradehub EXITED ({status}) while waiting for {what}: the restart loop this test \
                         guards against. stderr:\n{}",
                        self.log()
                    )
                }
            }
        }
    }

    /// The net position of the mounted symbol on the next summary line `hit` accepts.
    pub(crate) fn net_position(&mut self, what: &str, mut hit: impl FnMut(f64) -> bool) -> f64 {
        let line = self.wait_line(what, |line| summary_net_position(line).is_some_and(&mut hit));
        summary_net_position(&line).expect("a summary line")
    }

    fn alive(&mut self) -> bool {
        self.child.0.try_wait().expect("try_wait").is_none()
    }

    pub(crate) fn stop(mut self) {
        let status = Command::new("kill")
            .arg("-TERM")
            .arg(self.child.0.id().to_string())
            .status()
            .expect("kill(1)");
        assert!(status.success(), "kill -TERM failed: {status:?}");
        let ended = vike_log::poll::wait_until(PATIENCE, || {
            self.child.0.try_wait().expect("try_wait").is_some()
        });
        assert!(ended, "a stop ends the node within {PATIENCE:?}; stderr:\n{}", self.log());
    }
}

/// `net_pos` of a `"kind":"summary"` protocol line.
fn summary_net_position(line: &str) -> Option<f64> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    (value.get("kind")?.as_str()? == "summary").then(|| value.get("net_pos")?.as_f64()).flatten()
}

/// Push prices until the gatewayhub has seen `count` orders.
fn push_until_orders(hubs: &Hubs, count: usize, node: &Node) -> Vec<OrderRequest> {
    let placed = vike_log::poll::wait_until(PATIENCE, || {
        hubs.push_prices();
        std::thread::sleep(Duration::from_millis(500));
        hubs.orders().len() >= count
    });
    assert!(placed, "the account placed no order; stderr:\n{}", node.log());
    hubs.orders()
}

#[test]
fn a_tradehub_whose_every_account_fails_at_its_adapter_start_stays_up_and_the_account_joins_and_trades()
 {
    let hubs = Hubs::start(false);
    let root = temp_dir("core-no-engine");
    project(root.path(), &hubs);
    let mut node = Node::start(root.path());

    // 1. Every adapter fails: the node starts anyway, with no engine, and says why.
    node.wait_line("the ready line", |line| line.contains("\"kind\":\"ready\""));
    node.wait_log("NO account runs yet");
    node.wait_log("venue deribit not connected: venue unreachable");
    std::thread::sleep(Duration::from_secs(3));
    assert!(node.alive(), "the node with no engine stays up; stderr:\n{}", node.log());
    assert!(hubs.orders().is_empty(), "nothing traded before the account ran");

    // 2. The venue connects: the account watcher's next ask starts it, and its mount comes with it.
    hubs.venue_up.store(true, Ordering::SeqCst);
    node.wait_log("deribit STARTED in the running tradehub");
    node.wait_log("strategy mount(s) the start deactivated on it are mounted now");

    // 3. It trades: closed bars reach the mount, whose first bar buys at the gatewayhub.
    let orders = push_until_orders(&hubs, 1, &node);
    assert_eq!(orders[0].symbol.as_str(), SYMBOL);
    node.stop();
}

/// **A refusal parks** (owner 2026-11-16): the venue declines the account's key at its adapter start, so the node stays
/// up with no engine and says why, and the account watcher does NOT ask again by itself: the gatewayhub sees one start
/// across the backoff's first pauses (5 s, then 10 s, for a transient cause), and nothing trades.
#[test]
fn an_account_whose_key_the_venue_declines_parks_and_is_not_asked_again() {
    let hubs = Hubs::start(true);
    hubs.declined.store(true, Ordering::SeqCst);
    let root = temp_dir("core-no-engine-declined");
    project(root.path(), &hubs);
    let mut node = Node::start(root.path());
    node.wait_line("the ready line", |line| line.contains("\"kind\":\"ready\""));
    node.wait_log("venue deribit not connected: adapter failed");
    std::thread::sleep(Duration::from_secs(16));
    assert!(node.alive(), "the node stays up; stderr:\n{}", node.log());
    assert_eq!(
        hubs.starts.load(Ordering::SeqCst),
        1,
        "a refusal is asked once, never again by a timer; stderr:\n{}",
        node.log()
    );
    assert!(hubs.orders().is_empty(), "nothing traded");
    node.stop();
}

/// **The book comes back on a late join** (the rule that a refactor never changes working behaviour): the account
/// holds a position, the node restarts while its venue is down, and when the venue connects the account's engine has
/// the position it had, from the journal, as the boot would have restored it. Its strategy sees it and does not buy
/// again.
#[test]
fn an_account_that_joins_late_gets_back_the_position_it_held_before_the_restart() {
    let hubs = Hubs::start(true);
    let root = temp_dir("core-no-engine-journal");
    project(root.path(), &hubs);

    // Run 1: the venue is up; the strategy buys, the venue fills it, the node stops with the position.
    let mut node = Node::start(root.path());
    node.wait_line("the ready line", |line| line.contains("\"kind\":\"ready\""));
    let orders = push_until_orders(&hubs, 1, &node);
    hubs.fill(&orders[0]);
    let held = node.net_position("the filled position", |net| net != 0.0);
    assert!((held - orders[0].qty).abs() < 1e-9, "the fill is the position: {held}");
    node.stop();
    // The fake peer keeps a gone client's lease (a gatewayhub frees it when the socket ends): free it here.
    hubs.peer.kill_lease();

    // Run 2: the venue is DOWN at the boot: the node starts with no engine and holds the account's book.
    hubs.venue_up.store(false, Ordering::SeqCst);
    let mut node = Node::start(root.path());
    node.wait_line("the ready line", |line| line.contains("\"kind\":\"ready\""));
    node.wait_log("RESTORE ENGINE AWAITS ITS ACCOUNT deribit");
    assert_eq!(node.net_position("a summary", |_| true), 0.0, "nothing runs while it waits");

    // The venue connects: the account joins with the position it held, never a fresh, empty book.
    hubs.venue_up.store(true, Ordering::SeqCst);
    node.wait_log("RESTORE ON JOIN deribit");
    let back = node.net_position("the restored position", |net| net != 0.0);
    assert_eq!(back, held, "the position comes back as it was");
    // Its strategy reads the position it holds: bars reach it, and it buys nothing more.
    for _ in 0..6 {
        hubs.push_prices();
        std::thread::sleep(Duration::from_millis(500));
    }
    assert_eq!(hubs.orders().len(), 1, "no second buy: {:?}", hubs.orders());
    assert_eq!(node.net_position("a summary", |_| true), held);
    node.stop();
}
