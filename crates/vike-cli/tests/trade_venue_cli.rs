//! End-to-end proof that `vike-cli trade venue <book> balance|open-orders|positions` REACHES the wire: the shipped binary,
//! the observe handshake, `Request::VenueRead`, and the words and exit rungs an operator and a wrapper read.
//!
//! Hermetic and loopback-only, the `tests/trade_status_cli.rs` shape: ephemeral `127.0.0.1:0` ports, no
//! credentials, no venue, no external network, the observe key written into a throwaway settings database (the
//! STORE, not the environment). Two kinds of node: a SCRIPTED one (the real handshake, then the answer a test
//! scripts, recording every frame it was sent, so "nothing was sent" is an empty list) for the answers and the old
//! node, and the REAL paper node (`vike_tradehub::server`, no lease registry) for the one refusal the real server
//! makes without a venue, so the shipped client and the real server are proven to speak the same frames. The real
//! server answering over a lease registry is `crates/vike-tradehub/tests/daemon/venue_read.rs`'s.
//!
//! - [`a_balance_prints_the_figure_and_each_allowance`], [`the_open_orders_print_a_table_and_json_carries_the_payload`]:
//!   the feature, both reads, both output shapes, and the request the node was sent.
//! - [`a_refusal_is_the_nodes_sentence_and_exit_one`], [`the_real_node_without_a_registry_says_so`]: the node
//!   ANSWERED, so rung `1`, its words on stderr and NOTHING on stdout (never an empty answer).
//! - [`an_unreachable_node_is_rung_three`]: the far side never spoke.
//! - [`an_old_node_is_refused_client_side_and_nothing_is_sent`]: a node without `venue-read`.
//! - [`a_malformed_line_is_refused_before_anything_is_sent`]: rung `2`.

mod common;

use std::net::{SocketAddr, TcpListener};
use std::process::Command;
use std::sync::mpsc;
use std::thread;

use vike_hub_proto::auth::{HubKeys, Scope};
use vike_tradehub::{publish, server};
use vike_tradehub_client::proto::{
    FEATURE_VENUE_READ, FEATURE_VENUE_READ_POSITIONS, NODE_PROTO_VERSION, Request, Response,
    read_frame, write_frame,
};
use vike_tradehub_client::wire::{
    WireVenueAllowance, WireVenueBalance, WireVenueOpenOrder, WireVenuePosition, WireVenueRead,
    WireVenueReadKind,
};
use vike_tradehub_core::{MakerConfig, build_paper_maker_core};

const OBSERVE_KEY: &str = "DUMMY-observe-key-for-the-trade-venue-e2e";

/// A scripted node that advertises `features`, answers each request with `answer` (`None`: silence) and records
/// every frame after the handshake. It serves ONE connection; the log arrives when the client hangs up.
fn spawn_scripted_hub(
    features: Vec<String>,
    answer: impl Fn(&Request) -> Option<Response> + Send + 'static,
) -> (SocketAddr, mpsc::Receiver<Vec<Request>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut seen = Vec::new();
        if matches!(read_frame::<_, Request>(&mut stream), Ok(Request::Hello { .. })) {
            let welcome =
                Response::Welcome { proto_version: NODE_PROTO_VERSION, nonce: [7u8; 32], features };
            let _ = write_frame(&mut stream, &welcome);
            if matches!(read_frame::<_, Request>(&mut stream), Ok(Request::Auth { .. })) {
                // A scripted double: the mac is accepted unseen; what it exists to observe is what follows.
                let _ = write_frame(&mut stream, &Response::AuthOk { scope: Scope::Read });
                while let Ok(req) = read_frame::<_, Request>(&mut stream) {
                    if let Some(resp) = answer(&req) {
                        let _ = write_frame(&mut stream, &resp);
                    }
                    seen.push(req);
                }
            }
        }
        let _ = tx.send(seen);
    });
    (addr, rx)
}

fn venue_read_node(
    answer: impl Fn(&WireVenueReadKind) -> Response + Send + 'static,
) -> (SocketAddr, mpsc::Receiver<Vec<Request>>) {
    spawn_scripted_hub(
        vec![FEATURE_VENUE_READ.to_string(), FEATURE_VENUE_READ_POSITIONS.to_string()],
        move |req| match req {
            Request::VenueRead { kind, .. } => Some(answer(kind)),
            _ => None,
        },
    )
}

fn positions(rows: Vec<WireVenuePosition>) -> Response {
    Response::VenueRead(Box::new(WireVenueRead::Positions { positions: rows, truncated: false }))
}

fn held(title: &str, qty: f64, price: f64, redeemable: bool) -> WireVenuePosition {
    WireVenuePosition {
        symbol: "1484359444963781501765067677755406951259550728919736807361564486189514136251"
            .into(),
        title: title.into(),
        outcome: "Down".into(),
        qty,
        avg_price: Some(0.2099),
        price: Some(price),
        value: Some(price * qty),
        redeemable,
    }
}

fn balance() -> Response {
    Response::VenueRead(Box::new(WireVenueRead::Balance(WireVenueBalance {
        asset: "pUSD".into(),
        balance: 20.0,
        allowances: vec![WireVenueAllowance {
            spender: "0xE111180000d2663C0091e4f400237545B87B996B".into(),
            name: "CTF exchange".into(),
            amount: 0.0,
            unlimited: true,
        }],
    })))
}

fn orders() -> Response {
    Response::VenueRead(Box::new(WireVenueRead::OpenOrders {
        orders: vec![WireVenueOpenOrder {
            venue_order_id: "0xabc".into(),
            client_order_id: Some("pm-1".into()),
            symbol: "6710259892".into(),
            side: 1,
            price: Some(0.2),
            qty: 5.0,
            filled_qty: 0.0,
            status: "ACCEPTED".into(),
            ts_ms: 0,
        }],
        truncated: false,
    }))
}

struct Fixture {
    _root: tempfile::TempDir,
    settings: std::path::PathBuf,
}

/// A throwaway PROJECT whose `settings/` database holds the observe key (see `trade_status_cli.rs`'s
/// `SettingsFixture` for why the settings directory is a CHILD of the project).
fn fixture() -> Fixture {
    let root = tempfile::tempdir().expect("tempdir");
    let settings = root.path().join("settings");
    std::fs::create_dir_all(&settings).expect("create the settings directory");
    common::seed_store(&settings, &[("VIKE_TRADEHUB_OBSERVE_KEY", OBSERVE_KEY)]);
    Fixture { _root: root, settings }
}

struct Run {
    stdout: String,
    stderr: String,
    code: Option<i32>,
}

impl Run {
    fn transcript(&self) -> String {
        format!("--- stdout ---\n{}\n--- stderr ---\n{}", self.stdout, self.stderr)
    }
}

fn run_cli(addr: &str, fx: &Fixture, args: &[&str]) -> Run {
    let out = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .args(["trade", "venue"])
        .args(args)
        .args(["--node", addr])
        .env("VIKE_SETTINGS_DIR", &fx.settings)
        .env_remove("VIKE_USER_DATA_DIR")
        .env_remove("VIKE_TRADEHUB_OBSERVE_KEY")
        .env_remove("VIKE_TRADEHUB_CONTROL_KEY")
        .env_remove("VIKE_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
        .output()
        .expect("run vike-cli trade venue");
    Run {
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        code: out.status.code(),
    }
}

fn frames(rx: &mpsc::Receiver<Vec<Request>>) -> Vec<Request> {
    rx.recv_timeout(std::time::Duration::from_secs(10)).expect("the scripted node reports")
}

fn ask(venue: &str, account: &str, kind: WireVenueReadKind) -> Request {
    Request::VenueRead { venue: venue.into(), account: account.into(), kind }
}

#[test]
fn a_balance_prints_the_figure_and_each_allowance() {
    let (addr, rx) = venue_read_node(|_| balance());
    let run = run_cli(&addr.to_string(), &fixture(), &["polymarket", "balance"]);
    let t = run.transcript();
    assert_eq!(run.code, Some(0), "{t}");
    assert!(run.stdout.contains("polymarket balance: 20 pUSD"), "{t}");
    assert!(
        run.stdout.contains(
            "allowance to CTF exchange (0xE111180000d2663C0091e4f400237545B87B996B): unlimited"
        ),
        "{t}"
    );
    assert!(run.stderr.trim().is_empty(), "{t}");
    // The default account travels as the reserved word, the venue as typed, the kind as asked.
    assert_eq!(frames(&rx), [ask("polymarket", "DEFAULT", WireVenueReadKind::Balance)]);
}

#[test]
fn the_open_orders_print_a_table_and_json_carries_the_payload() {
    let fx = fixture();
    let (addr, _rx) = venue_read_node(|_| orders());
    let human = run_cli(&addr.to_string(), &fx, &["polymarket", "open-orders"]);
    let t = human.transcript();
    assert_eq!(human.code, Some(0), "{t}");
    assert!(human.stdout.contains("polymarket open orders at the venue: 1"), "{t}");
    for cell in ["ORDER ID", "0xabc", "buy", "0.2", "ACCEPTED", "6710259892", "pm-1"] {
        assert!(human.stdout.contains(cell), "`{cell}` in {t}");
    }

    let (addr, rx) = venue_read_node(|_| orders());
    let json = run_cli(&addr.to_string(), &fx, &["hyperliquid/TESTNET", "open-orders", "--json"]);
    let t = json.transcript();
    assert_eq!(json.code, Some(0), "{t}");
    let v: serde_json::Value = serde_json::from_str(&json.stdout)
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}):\n{t}"));
    assert_eq!(
        (v["venue"].as_str(), v["account"].as_str(), v["kind"].as_str()),
        (Some("hyperliquid"), Some("TESTNET"), Some("open-orders")),
        "{t}"
    );
    assert_eq!(v["read"]["OpenOrders"]["orders"][0]["venue_order_id"], "0xabc", "{t}");
    assert_eq!(v["read"]["OpenOrders"]["truncated"], false, "{t}");
    // A labelled book is addressed by its label.
    assert_eq!(frames(&rx), [ask("hyperliquid", "TESTNET", WireVenueReadKind::OpenOrders)]);
}

/// The third read: the table is in words with the redeemable flag and its note, JSON names the kind and
/// carries the payload verbatim, and an empty wallet is an answer on stdout with exit 0.
#[test]
fn the_positions_print_a_table_json_carries_the_payload_and_an_empty_wallet_is_an_answer() {
    let fx = fixture();
    let rows = || {
        vec![
            held("Solana Up or Down - June 9, 3:40AM-3:45AM ET", 11.11, 0.0, true),
            held("BNB Up or Down - June 12, 4:30AM-4:45AM ET", 5.0, 1.0, true),
        ]
    };
    let (addr, rx) = venue_read_node(move |_| positions(rows()));
    let human = run_cli(&addr.to_string(), &fx, &["polymarket", "positions"]);
    let t = human.transcript();
    assert_eq!(human.code, Some(0), "{t}");
    assert!(human.stdout.contains("polymarket positions at the venue: 2"), "{t}");
    for cell in ["REDEEMABLE", "Solana Up or Down", "BNB Up or Down", "11.11", "0.2099", "yes"] {
        assert!(human.stdout.contains(cell), "`{cell}` in {t}");
    }
    assert!(human.stdout.contains("A losing outcome says yes too and pays nothing"), "{t}");
    assert!(human.stderr.trim().is_empty(), "{t}");
    assert_eq!(frames(&rx), [ask("polymarket", "DEFAULT", WireVenueReadKind::Positions)]);

    let (addr, _rx) = venue_read_node(move |_| positions(rows()));
    let json = run_cli(&addr.to_string(), &fx, &["polymarket", "positions", "--json"]);
    let t = json.transcript();
    assert_eq!(json.code, Some(0), "{t}");
    let v: serde_json::Value = serde_json::from_str(&json.stdout)
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}):\n{t}"));
    assert_eq!(v["kind"].as_str(), Some("positions"), "{t}");
    assert_eq!(v["read"]["Positions"]["positions"][1]["redeemable"], true, "{t}");
    assert_eq!(v["read"]["Positions"]["positions"][1]["value"], 5.0, "{t}");

    let (addr, _rx) = venue_read_node(|_| positions(Vec::new()));
    let empty = run_cli(&addr.to_string(), &fx, &["polymarket", "positions"]);
    let t = empty.transcript();
    assert_eq!(empty.code, Some(0), "an empty wallet is an answer:\n{t}");
    assert!(empty.stdout.contains("the wallet holds nothing"), "{t}");
}

/// ⚠ A node from the build that has `venue-read` but not the positions kind cannot decode the kind's name:
/// the client refuses `positions` locally, naming `venue-read-positions`, and sends NOTHING; the same node is
/// still asked for a balance.
#[test]
fn positions_against_a_node_with_only_the_older_word_is_refused_and_nothing_is_sent() {
    let only_older = || {
        spawn_scripted_hub(vec![FEATURE_VENUE_READ.to_string()], |req| match req {
            Request::VenueRead { .. } => Some(balance()),
            _ => None,
        })
    };
    let (addr, rx) = only_older();
    let run = run_cli(&addr.to_string(), &fixture(), &["polymarket", "positions"]);
    let t = run.transcript();
    assert_eq!(run.code, Some(1), "{t}");
    assert!(run.stderr.contains("venue-read-positions"), "{t}");
    assert!(run.stderr.contains("older vike-tradehub"), "{t}");
    let seen = frames(&rx);
    assert!(seen.is_empty(), "a frame arrived after the handshake: {seen:?}\n{t}");

    let (addr, _rx) = only_older();
    let run = run_cli(&addr.to_string(), &fixture(), &["polymarket", "balance"]);
    assert_eq!(run.code, Some(0), "the older kinds still go:\n{}", run.transcript());
}

#[test]
fn a_refusal_is_the_nodes_sentence_and_exit_one() {
    let sentence = "not offered by binance: its bridge has no account-wide open-orders read";
    let (addr, _rx) = venue_read_node(move |_| Response::Error(sentence.into()));
    let run = run_cli(&addr.to_string(), &fixture(), &["binance", "open-orders"]);
    let t = run.transcript();
    assert_eq!(run.code, Some(1), "a node that ANSWERED is never the connect rung:\n{t}");
    assert!(run.stderr.contains(sentence), "{t}");
    assert!(run.stdout.trim().is_empty(), "a read that could not be made prints no answer:\n{t}");
}

/// The REAL server, with no lease registry (a paper-only node): the shipped client and the real server agree on
/// the frames, and the refusal is the server's own sentence.
#[test]
fn the_real_node_without_a_registry_says_so() {
    let cfg = MakerConfig::outcome_token("polymarket", "CLI_TRADE_VENUE_E2E", Some(3_000_000_000));
    let mount = build_paper_maker_core(&cfg);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let publisher = publish::spawn(mount.handle.snapshot_cell(), None);
    let keys = HubKeys::new(OBSERVE_KEY.as_bytes().to_vec(), Vec::new());
    thread::spawn(move || {
        let _ = server::serve(
            listener,
            publisher,
            keys,
            None,
            server::control::ControlLimitsConfig::default(),
            None,
            None,
            None,
        );
    });
    let run = run_cli(&addr.to_string(), &fixture(), &["polymarket", "balance"]);
    let t = run.transcript();
    assert_eq!(run.code, Some(1), "{t}");
    assert!(run.stderr.contains("no trade lease registry"), "{t}");
    assert!(run.stdout.trim().is_empty(), "{t}");
    drop(mount);
}

#[test]
fn an_unreachable_node_is_rung_three() {
    // A port that was bound and released: nothing listens on it.
    let addr = {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        l.local_addr().expect("addr")
    };
    let run = run_cli(&addr.to_string(), &fixture(), &["polymarket", "balance"]);
    let t = run.transcript();
    assert_eq!(run.code, Some(3), "{t}");
    assert!(run.stderr.contains("cannot query"), "{t}");
}

#[test]
fn an_old_node_is_refused_client_side_and_nothing_is_sent() {
    let (addr, rx) =
        spawn_scripted_hub(vec!["subscribe".to_string(), "directory".to_string()], |_| None);
    let run = run_cli(&addr.to_string(), &fixture(), &["polymarket", "balance"]);
    let t = run.transcript();
    assert_eq!(run.code, Some(1), "{t}");
    assert!(run.stderr.contains("venue-read") && run.stderr.contains("older vike-tradehub"), "{t}");
    assert!(run.stderr.contains("upgrade the node at"), "{t}");
    let seen = frames(&rx);
    assert!(seen.is_empty(), "a frame arrived after the handshake: {seen:?}\n{t}");
}

#[test]
fn a_malformed_line_is_refused_before_anything_is_sent() {
    // Nothing listens at the address: a refusal that tried to dial would be rung 3, not 2.
    let fx = fixture();
    for args in
        [&["polymarket", "fills"][..], &["polymarket#ALT", "balance"][..], &["polymarket"][..]]
    {
        let run = run_cli("127.0.0.1:1", &fx, args);
        assert_eq!(run.code, Some(2), "{args:?}:\n{}", run.transcript());
    }
}
