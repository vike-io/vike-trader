//! **`vike-cli backtest run --json` prints the bytes it printed before the msgpack body** ("JSON only at the edges",
//! 2026-11-19): the CLI's client speaks msgpack to a backtesthub that advertises it (every one of this tree:
//! `crates/vike-backtesthub-client/tests/msgpack_negotiation.rs` holds the default), and the report arrives as its
//! tree and is read back into the very text, which the CLI prints verbatim; a summary arrives as its value and is
//! printed by `serde_json`. The golden is what a JSON-only client of the same daemon reads, so a script's bytes do
//! not move.
//!
//! Hermetic and loopback-only, the shipped bin against the REAL backtesthub over a seeded in-memory store.

use std::net::SocketAddr;
use std::process::Command;
use std::sync::Arc;

use vike_backtesthub::test_support::spawn_server_over;
use vike_backtesthub_client::BacktesthubClient;
use vike_backtesthub_client::client::ParamscanProfileAnswer;
use vike_backtesthub_client::summary::{ParamscanProfileSummaryWire, RunSummaryWire};
use vike_data::{HistoryStore, HistoryStoreMemory};
use vike_hub_protocol::frame::BodyCodec;
use vike_marketdata::Bar;

/// A backtesthub over 400 one-minute BTCUSDT bars of a seeded walk.
fn seeded_server() -> SocketAddr {
    let mut seed: u64 = 0x1234_5678;
    let mut price = 100.0f64;
    let bars: Vec<Bar> = (0..400)
        .map(|i| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            price = (price + ((seed >> 32) as f64 / f64::from(u32::MAX) - 0.5) * 2.0).max(1.0);
            Bar {
                symbol: Some("BTCUSDT".into()),
                ..Bar::ohlcv(60_000 * (i + 1), price, price, price, price, 1.0)
            }
        })
        .collect();
    let store = HistoryStoreMemory::new();
    store.append_bars("binance", "BTCUSDT", "1m", &bars, None).expect("seed");
    spawn_server_over(Arc::new(store), vike_strategy_lab::stand_in)
}

/// A buy-and-hold profile over the seeded bars, with `extra` appended.
fn profile(extra: &str) -> String {
    format!(
        "name = \"msgpack_body_cli\"\n\n[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\nkind = \"bar\"\n\
         interval = \"1m\"\nfrom = \"0\"\nto = \"100000\"\n\n[engine]\ncash = 1000.0\n\n[strategy]\n\
         file = \"trend/buy_hold.rs\"\n[strategy.params]\nsize = 1.0\nsymbol = \"BTCUSDT\"\n{extra}"
    )
}

/// The shipped bin's stdout for `backtest run --profile <profile> --addr <addr> --json [extra…]`.
fn cli_json(addr: SocketAddr, profile: &str, extra: &[&str]) -> String {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("profile.toml");
    std::fs::write(&path, profile).expect("write the profile");
    let out = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .args(["backtest", "run", "--profile"])
        .arg(&path)
        .args(["--addr", &addr.to_string(), "--json"])
        .args(extra)
        .output()
        .expect("run vike-cli");
    assert!(out.status.success(), "vike-cli exits 0: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("UTF-8 stdout")
}

/// A JSON-only client of the daemon: what every client read before the msgpack body.
fn json_client(addr: SocketAddr) -> BacktesthubClient {
    let mut client = BacktesthubClient::connect(addr).expect("connect");
    assert_eq!(client.prefer_body(BodyCodec::Json), BodyCodec::Json);
    client
}

/// The whole reports (a single run's and a scan's `--whole`) print the very text a JSON client reads, and the summaries
/// (a single run's, a scan's) the very `serde_json` of the summary a JSON client reads (each run held under its own name,
/// the one field that tells two runs apart). MUTATION PROOF: write a report's tree back through `serde_json::Value`
/// (sorted keys) and the scan's and the single run's lines differ. No walk-forward: over these bars the harness panics
/// in `walk_forward_over_windows` (empty splits), a fault of the walk, not of the wire.
#[test]
fn the_json_a_script_reads_is_byte_identical_over_the_msgpack_body() {
    let addr = seeded_server();
    let single = profile("");
    let scan = profile("\n[paramscan]\nsize = [1.0, 2.0, 3.0]\n");
    let mut client = json_client(addr);
    let expected = [
        (cli_json(addr, &single, &["--whole"]), client.run_backtest(&single).expect("a report")),
        (
            cli_json(addr, &scan, &["--whole"]),
            client.run_paramscan_profile(&scan, None, None).expect("a scan"),
        ),
    ];
    for (printed, report) in expected {
        assert_eq!(printed, format!("{report}\n"));
    }
    let printed = cli_json(addr, &single, &[]);
    let summary = client.run_backtest_summary(&single).expect("a summary");
    let held: RunSummaryWire = serde_json::from_str(&printed).expect("the CLI printed a summary");
    let golden = RunSummaryWire { run: held.run, ..summary };
    assert_eq!(printed, format!("{}\n", serde_json::to_string(&golden).expect("encodes")));
    // A scan's summary (decision 0078's amendment of 2026-11-19): its rows' metric tables crossed msgpack as
    // columns and print as the JSON client's.
    let printed = cli_json(addr, &scan, &[]);
    let ParamscanProfileAnswer::Summary(summary) =
        client.run_paramscan_profile_answer(&scan, None, None, false).expect("a scan's summary")
    else {
        panic!("this backtesthub summarizes a scan");
    };
    let held: ParamscanProfileSummaryWire =
        serde_json::from_str(&printed).expect("the CLI printed a scan's summary");
    let golden = ParamscanProfileSummaryWire { run: held.run, ..*summary };
    assert_eq!(printed, format!("{}\n", serde_json::to_string(&golden).expect("encodes")));
}
