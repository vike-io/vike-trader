//! A dial of a node takes the box's patience: `crates/vike-cli/src/cmd/hub_dial_tests.rs`'s
//! `no_production_dial_in_this_crate_bypasses_the_box_patience` for this crate's own sources. A read
//! of a hub answers inside `HUB_READ_WAIT`, because the whole server waits behind it.

use std::io::Read;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use vike_hub_protocol::frame::write_frame;

use super::cancellation::WAIT;
use super::*;

/// A hub that answers the handshake and then says nothing more: a half-open link, as a dead tunnel
/// leaves it. Takes its protocol's own `Welcome`, because the two hubs share no type.
fn silent_hub(welcome: impl serde::Serialize + Send + 'static) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr").to_string();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("the tool dials");
        write_frame(&mut stream, &welcome).expect("the welcome");
        let mut sink = [0u8; 256];
        while matches!(stream.read(&mut sink), Ok(n) if n > 0) {}
    });
    addr
}

/// **`list_series` and `list_strategies` are answered inline, so on a half-open link everything behind
/// them waits with them**: a `market_exit` included, and a cancel is not read. A metadata read is
/// bounded in seconds; each failing here means the agent's next call waited five minutes.
#[test]
fn a_hub_read_gives_up_on_a_silent_link_inside_its_bound() {
    use vike_backtesthub_client::proto as backtest;
    use vike_datahub_client::proto as data;

    let datahub = silent_hub(data::Response::Welcome {
        proto_version: vike_datahub_client::PROTO_VERSION,
        features: Vec::new(),
        nonce: None,
    });
    let backtesthub = silent_hub(backtest::Response::Welcome {
        proto_version: backtest::PROTO_VERSION,
        features: Vec::new(),
        nonce: None,
    });
    // The bound is the router's to pass, `HUB_READ_WAIT`; a second here so the case takes one.
    let bound = Duration::from_secs(1);
    let (answered, answers) = mpsc::channel();
    let series = answered.clone();
    thread::spawn(move || {
        let _ = series.send(("list_series", tool_list_series(&datahub, None, bound).map(drop)));
    });
    thread::spawn(move || {
        let strategies = tool_list_strategies(&backtesthub, None, bound).map(drop);
        let _ = answered.send(("list_strategies", strategies));
    });
    for _ in 0..2 {
        let (tool, answer) = answers
            .recv_timeout(bound + WAIT)
            .expect("a read on a silent link holds the whole server past its bound");
        let why = answer.expect_err("the hub said nothing, so there is nothing to answer with");
        assert!(why.contains("no reply byte"), "{tool}: {why}");
    }
}

/// `connect` is the default-window door for a caller with no setting to read; a tool that dialled
/// through it would silently ignore both rows. Scans this crate's non-test sources, comments excluded.
#[test]
fn no_production_dial_in_this_crate_bypasses_the_box_patience() {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read a source directory").flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                if !name.ends_with("_tests") && name != "tests" {
                    walk(&path, out);
                }
            } else if name.ends_with(".rs") && !name.ends_with("_tests.rs") {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
    assert!(files.len() > 15, "the walk found {} files — wrong place", files.len());
    // Assembled: a literal would still read as data to a search.
    let needles = [
        concat!("TradehubCoreHandle", "::connect("),
        concat!("TradehubControlHandle", "::connect("),
    ];
    for file in files {
        let text = std::fs::read_to_string(&file).expect("read a source file");
        for (n, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            for needle in needles {
                assert!(
                    !line.contains(needle),
                    "{}:{} dials through `connect`, which ignores the box's link patience; use \
                     connect_with_read_timeout / connect_with_reply_timeout with \
                     `HubKeyring::patience`",
                    file.display(),
                    n + 1
                );
            }
        }
    }
}
