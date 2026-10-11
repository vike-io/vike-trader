//! `data realtime`: `watch` streaming off a mounted market-data plane, and `status`.

use std::io::Write;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use vike_data::{DataClient, HistStore, LiveDataError, LiveDataSink, MemHistStore, SubscriptionId};
use vike_datahub::md::MdHub;
use vike_datahub::serve_authed;
use vike_datahub_client::{
    FEATURE_MARKET_DATA, MdBye, PROTO_VERSION,
    market::{MdFrame, MdSessionId},
    md_venue_feature,
    proto::{Request, Response, read_frame, write_frame},
};
use vike_model::TradeTick;

use super::support::{jsonl_rows, spawn_seeded_datahub};
use super::*;

/// The venue feed [`spawn_md_datahub`] streams from: a `DataClient` that answers `subscribe_trades`
/// by pushing prints into the hub's sink on a thread of its own.
///
/// ⚠ **It is a producer rather than a recorder**, unlike `crates/vike-datahub/tests/md_hub.rs`'s
/// `ScriptedFeed`, and that is what this file needs: the property under test here is that a print
/// crosses the whole path — sink → publish tick → mailbox → socket → `read_frame` → a rendered row
/// on stdout — which no double that only records its calls can exercise.
///
/// The push is BOUNDED (a fixed number of prints on a 20 ms cadence) so the thread ends on its own:
/// a test binary that leaves an endless producer running pays for it in every later case.
struct PushFeed {
    venue: String,
    sink: Arc<dyn LiveDataSink>,
}

impl DataClient for PushFeed {
    fn subscribe_bars(&mut self, _s: &str, _i: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("this fixture serves no bars"))
    }
    fn subscribe_quotes(&mut self, _s: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("this fixture serves no quotes"))
    }
    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        let sink = Arc::clone(&self.sink);
        let venue = self.venue.clone();
        let symbol = symbol.to_string();
        thread::spawn(move || {
            for i in 0..300i64 {
                sink.trade(
                    &venue,
                    &symbol,
                    TradeTick {
                        ts: 1_700_000_000_000 + i,
                        local_ts: 0,
                        price: 100.0 + i as f64,
                        size: 0.5,
                        // Alternating, so a renderer that hard-coded one side would show it.
                        is_buyer_maker: i % 2 == 0,
                        // ⚠ EMPTY, as the hub itself leaves it — `MdFrame::Trades`' own doc: the
                        // envelope's symbol is authoritative and the per-tick one is blanked on the
                        // way in. A fixture that filled it would hide the mis-key the renderer is
                        // written to avoid.
                        symbol: String::new(),
                    },
                );
                thread::sleep(Duration::from_millis(20));
            }
        });
        Ok(SubscriptionId(1))
    }
    fn subscribe_book(&mut self, _s: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("this fixture serves no book"))
    }
    fn unsubscribe(&mut self, _id: SubscriptionId) {}
    fn shutdown(&mut self) {}
}

/// A datahub with the MARKET-DATA plane MOUNTED over [`PushFeed`], advertising `binance` alone.
///
/// ⚠ The venue and the lane are not arbitrary: `vike_model::venues::venue_caps`' binance row declares
/// `trades: true`, and the hub refuses a lane a venue's declared caps do not serve through
/// `vike_data::require_live_verb` — so a fixture naming any other pair would be refused by the
/// server before a frame could prove anything.
fn spawn_md_datahub() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    let hub = MdHub::new(
        Box::new(|venue: &str, sink: Arc<dyn LiveDataSink>| {
            Ok(Box::new(PushFeed { venue: venue.to_string(), sink }) as Box<dyn DataClient + Send>)
        }),
        vec!["binance".to_string()],
    );
    hub.spawn();
    thread::spawn(move || {
        let _ = serve_authed(listener, store, None, None, Some(hub), None, None);
    });
    addr
}

/// **A PRINT CROSSES THE WHOLE PATH**, and the stream stops where it was told to.
///
/// End to end: a venue feed's `TradeTick` → the hub's sink → the publish tick → this subscriber's
/// mailbox → the socket → `read_frame` → a row on stdout. Nothing below is provable from a parser
/// test, and the `--events` bound is what makes a live stream assertable at all.
///
/// ⚠ `--for` rides beside `--events` as a FAILSAFE rather than as the property under test: without
/// it a broken path would park in the read deadline the server's own heartbeat period armed (45s at
/// the floor) before failing. With it, a stream that delivers nothing ends on the time bound —
/// still exit 0, since the bound WAS reached — and the row assertions below are what redden.
#[test]
fn watch_streams_frames_to_stdout_and_stops_at_the_events_bound() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_md_datahub().to_string();
    let out = run(
        scratch.path(),
        &[
            "data",
            "realtime",
            "watch",
            "binance:BTCUSDT",
            "--lane",
            "trades",
            "--events",
            "2",
            "--for",
            "20s",
            "--format",
            "jsonl",
            "--addr",
            &addr,
        ],
    );
    assert_eq!(out.status.code(), Some(0), "the bound was reached: {}", stderr(&out));
    let text = stdout(&out);
    let rows = jsonl_rows(&text);
    let trades: Vec<&serde_json::Value> = rows.iter().filter(|r| r["type"] == "trades").collect();
    assert!(
        trades.len() >= 2,
        "the --events bound is TWO data frames, and every one of them must be on stdout: {text}"
    );
    let first = trades[0];
    assert_eq!(first["venue"], "binance");
    // ⚠ The ENVELOPE's symbol, on a fixture whose ticks carry an empty one. A renderer that
    // serialized `TradeTick` would put `""` here and anything grouping by it would fold every
    // venue's tape into one bucket.
    assert_eq!(first["symbol"], "BTCUSDT", "{first}");
    let prints = first["prints"].as_array().expect("a trades frame carries its prints");
    assert!(!prints.is_empty(), "{first}");
    assert!(prints[0]["price"].is_number(), "{first}");
    assert!(prints[0].get("symbol").is_none(), "no empty per-tick symbol rides as an answer");

    // **STDOUT CARRIES FRAMES AND NOTHING ELSE.** The subscription note and the closing summary are
    // this binary's own prose and belong on stderr, or a `| jq` chokes on them.
    assert!(!text.contains("watching binance"), "the subscription note is not a frame: {text}");
    assert!(!text.contains("stream ended"), "the summary is not a frame: {text}");
    let err = stderr(&out);
    assert!(err.contains("watching binance BTCUSDT"), "…and it is still SAID, on stderr: {err}");
    assert!(err.contains("stream ended"), "{err}");
    assert!(err.contains("data,"), "the summary counts by class: {err}");

    // ...and `--out FILE` moves exactly that stream off stdout, leaving it EMPTY — which is what
    // makes the file a tape rather than a transcript.
    //
    // ⚠ **Its OWN datahub, and that is not tidiness — reusing the first one is a TIMER.**
    // `PushFeed::subscribe_trades` pushes 300 ticks at 20 ms and ends, and it runs ONCE per venue
    // subscription: `MdHub` holds the key through `MD_LINGER` after the first session closes, so
    // re-acquiring it calls no second `subscribe_trades` and the second run rides the FIRST run's
    // producer. On a loaded box, a first run plus process teardown plus this spawn taking longer
    // than that ~6 s life leaves this watch subscribed to a finished producer: nothing arrives, the
    // `--for 20s` failsafe ends it at exit 0, and the row assertion below reddens for a reason that
    // has nothing to do with `--out`. A fresh hub is a fresh producer with its own clock.
    let out_addr = spawn_md_datahub().to_string();
    let tape = scratch.path().join("tape.jsonl");
    let tape_arg = tape.to_string_lossy().to_string();
    let out = run(
        scratch.path(),
        &[
            "data",
            "realtime",
            "watch",
            "binance:BTCUSDT",
            "--lane",
            "trades",
            "--events",
            "1",
            "--for",
            "20s",
            "--out",
            &tape_arg,
            "--addr",
            &out_addr,
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), "", "with --out, stdout carries nothing at all");
    let written = std::fs::read_to_string(&tape).expect("the tape file");
    let file_rows = jsonl_rows(&written);
    assert!(
        file_rows.iter().any(|r| r["type"] == "trades"),
        "the frames went to the file: {written}"
    );
    // The default rendering follows the DESTINATION: a file is not a terminal, so it got the
    // machine form without being asked — which is what `jsonl_rows` above just proved by parsing it.
    assert!(
        !written.contains("trades binance BTCUSDT seq="),
        "a file must not get the human table form: {written}"
    );
}

/// A datahub that HANDSHAKES correctly and then MISBEHAVES: it answers `Hello` and `MdSubscribe`
/// exactly as the real server does — advertising the market-data plane and `binance`, and accepting
/// the spec verbatim — and then hands the socket to `after`, which is where the fault under test is
/// planted.
///
/// ⚠ Hand-rolled rather than [`spawn_md_datahub`], and the reason is the property: what this binary
/// does when the WIRE breaks cannot be driven from a server that works, and a real one does not
/// break on request. Everything up to the last frame is the real exchange, so the CLI reaches
/// `stream_frames` by the ordinary path.
fn spawn_scripted_md_server(after: fn(&mut TcpStream)) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    thread::spawn(move || {
        let (mut s, _) = listener.accept().expect("the CLI dials immediately");
        let _: Request = read_frame(&mut s).expect("the client opens with Hello");
        write_frame(
            &mut s,
            &Response::Welcome {
                proto_version: PROTO_VERSION,
                features: vec![FEATURE_MARKET_DATA.to_string(), md_venue_feature("binance")],
                nonce: None,
            },
        )
        .expect("the welcome");
        let specs = match read_frame::<_, Request>(&mut s).expect("the subscription") {
            Request::MdSubscribe { specs } => specs,
            _ => panic!("the CLI's next frame after the handshake is MdSubscribe"),
        };
        write_frame(
            &mut s,
            &Response::MdSubscribed {
                session: MdSessionId(1),
                // Accepted VERBATIM: nothing here is testing a clamp or a refusal.
                accepted: specs,
                refused: Vec::new(),
                heartbeat_ms: 1_000,
            },
        )
        .expect("the acceptance");
        after(&mut s);
    });
    addr
}

/// **A BROKEN STREAM IS NOT A FINISHED ONE, and `--unbounded` does not change that** — end to end,
/// in the one channel a wrapper reads.
///
/// ⚠ This is the case for a defect that was PINNED: `End::was_asked_for` folded every non-bound end
/// together under `--unbounded`, so a transport fault and a protocol desync exited 0, identically
/// to a clean stop. A wrapper running `… --unbounded --out tape.jsonl` under `set -e` took a
/// half-written tape for a complete capture. The unit suite beside
/// `crates/vike-cli/src/cmd/data/realtime/watch.rs`'s `End` drives the classification over a real socket;
/// this is the half that proves it reaches the EXIT CODE.
#[test]
fn an_unbounded_watch_that_faults_exits_non_zero() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let tape = scratch.path().join("broken.jsonl");
    let tape_arg = tape.to_string_lossy().to_string();

    // A TRANSPORT FAULT: a well-framed body that does not decode, which is how one reaches this
    // verb (`read_frame` fuses framing and decoding into one `InvalidData`).
    let broken = spawn_scripted_md_server(|s| {
        s.write_all(&3u32.to_be_bytes()).expect("a valid length prefix");
        s.write_all(b"{{{").expect("...and a body that is not JSON");
        s.flush().expect("the scripted bytes are on the wire");
    })
    .to_string();
    let out = run(
        scratch.path(),
        &[
            "data",
            "realtime",
            "watch",
            "binance:BTCUSDT",
            "--lane",
            "trades",
            "--unbounded",
            "--out",
            &tape_arg,
            "--addr",
            &broken,
        ],
    );
    let err = stderr(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a faulted stream may not exit 0 — a wrapper cannot tell it from a finished capture: {err}"
    );
    assert!(err.contains("the stream FAILED"), "…and it says which of the two it was: {err}");

    // THE CONTROL, and it is what stops the case above passing because this harness always fails:
    // the SAME line against a server that says GOODBYE — an ENDING rather than a break — is exit 0,
    // because `--unbounded` named no bound to miss.
    let polite = spawn_scripted_md_server(|s| {
        write_frame(s, &Response::Md(Box::new(MdFrame::Bye(MdBye::ServerStopping))))
            .expect("the goodbye");
    })
    .to_string();
    let out = run(
        scratch.path(),
        &[
            "data",
            "realtime",
            "watch",
            "binance:BTCUSDT",
            "--lane",
            "trades",
            "--unbounded",
            "--addr",
            &polite,
        ],
    );
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(0), "a goodbye ENDED the stream: {err}");
    assert!(err.contains("the server ended the stream"), "{err}");
}

/// A server with NO market-data plane is a RUN failure naming the capability, and nothing is
/// streamed.
///
/// ⚠ The rung is the run-failure one rather than connect-class: the socket was fine and the
/// capability was missing, which is a served answer — the same split
/// `crates/vike-cli/src/cmd/data/shared.rs`'s `connect` already makes for the `hist` read verbs. And the
/// refusal is LOCAL: `DatahubClient::md_subscribe` checks the advertisement before sending, so the
/// message names the key an operator has to set on the SERVER.
#[test]
fn watch_against_a_server_with_no_market_data_plane_names_the_capability() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();
    let out = run(
        scratch.path(),
        &[
            "data",
            "realtime",
            "watch",
            "binance:BTCUSDT",
            "--lane",
            "trades",
            "--events",
            "1",
            "--addr",
            &addr,
        ],
    );
    assert_eq!(out.status.code(), Some(1), "a server that ANSWERED is the run-failure rung");
    let err = stderr(&out);
    assert!(err.contains("market_data"), "the refusal names the missing capability: {err}");
    assert_eq!(stdout(&out), "", "nothing was printed as though a frame had arrived");
}

/// **`status` REPORTS AN ADVERTISEMENT AND SAYS SO**, in both renderings and in both server shapes.
///
/// The pairing is the whole test: a datahub with the plane mounted lists the venue it advertises,
/// one without it says NOT advertised — and neither is allowed to read as a health check, because
/// nothing here observed a frame. §11's word for this verb is "health", which is exactly why the
/// output has to contradict that expectation in its own words.
#[test]
fn status_reports_an_advertisement_and_never_a_liveness_verdict() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let serving = spawn_md_datahub().to_string();
    let bare = spawn_seeded_datahub().to_string();

    let out = run(scratch.path(), &["data", "realtime", "status", "--addr", &serving]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stdout(&out);
    // ⚠ **THE TABLE IS A TABLE**, asserted before anything is read out of it. A child's stdout is
    // never a terminal, so a default that followed the destination answered here in JSON — and
    // every assertion below passed on that document's own keys (`market_data_advertised` contains
    // `advertised`; the venue is a string in it). This line is what makes the rest mean what they
    // say; `default_render`'s doc carries the incident.
    assert!(!text.trim_start().starts_with('{'), "the default answer is a table, not JSON: {text}");
    assert!(
        text.lines().any(|l| l.starts_with("binance")),
        "the advertised venue is a ROW of its own: {text}"
    );
    assert!(text.contains("LIVE FEED"), "…under the column that says what it is: {text}");
    assert!(text.contains("not a liveness check"), "{text}");
    // The claim it may never make. `healthy` is what a reader expects from the word `status`, and
    // printing it about a venue nothing probed is positive confirmation of something false.
    assert!(!text.to_lowercase().contains("healthy"), "nothing here measured health: {text}");

    let doc: serde_json::Value = serde_json::from_str(&stdout(&run(
        scratch.path(),
        &["data", "realtime", "status", "--json", "--addr", &serving],
    )))
    .expect("stdout is one JSON document");
    assert_eq!(doc["market_data_advertised"], true);
    assert_eq!(doc["venues"][0], "binance");
    assert_eq!(doc["count"], 1);
    assert_eq!(doc["liveness_probed"], false, "the field a consumer must not assume away: {doc}");

    // The OTHER shape — and it is what stops the assertions above passing on a verb that says
    // `advertised` whatever it was told.
    let out = run(scratch.path(), &["data", "realtime", "status", "--addr", &bare]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(!text.trim_start().starts_with('{'), "a table here too: {text}");
    assert!(text.contains("NOT advertised"), "{text}");
    assert!(text.contains("market_data"), "it names the capability that is missing: {text}");
    assert!(text.contains("no venue advertises"), "{text}");
    assert!(text.contains("not a liveness check"), "every answer ends on it: {text}");
}

/// Every `data realtime` line an operator gets wrong is caught BEFORE a socket is opened — which is
/// what the USAGE rung means here, and what keeps a wrong command line costing nothing.
///
/// ⚠ None of these names an `--addr`, so a case that reached the dial would try the default
/// 127.0.0.1:7878 — a real datahub on a deployed box. Exit 2 is the proof that none of them does:
/// `parse` runs before `connect`, and a connect-class failure is rung 3.
#[test]
fn a_bad_realtime_command_line_is_the_usage_rung() {
    let scratch = tempfile::tempdir().expect("tempdir");
    for (args, needle) in [
        (vec!["data", "realtime", "watch", "binance:BTCUSDT", "--lane", "trades"], "BOUNDED"),
        (vec!["data", "realtime", "watch", "binance:BTCUSDT", "--events", "1"], "needs --lane"),
        (
            vec!["data", "realtime", "watch", "b:S", "--lane", "quotes", "--events", "1"],
            "no quotes lane",
        ),
        (
            vec![
                "data",
                "realtime",
                "watch",
                "binance:BTCUSDT:1h",
                "--lane",
                "depth",
                "--events",
                "1",
            ],
            "INTERVAL",
        ),
        (
            vec![
                "data", "realtime", "watch", "b:S", "--lane", "trades", "--depth", "5", "--events",
                "1",
            ],
            "--depth does not apply",
        ),
        (
            vec!["data", "realtime", "watch", "b:S", "--lane", "trades", "--events", "1", "--json"],
            "jsonl",
        ),
        (vec!["data", "realtime", "watch", "b:S", "--lane", "trades", "--for", "3d"], "record"),
        (
            vec![
                "data",
                "realtime",
                "watch",
                "b:S",
                "--lane",
                "trades",
                "--unbounded",
                "--events",
                "1",
            ],
            "contradicts",
        ),
        (vec!["data", "realtime", "status", "--lane", "trades"], "does not apply to `status`"),
        (vec!["data", "realtime", "status", "--format", "jsonl"], "ONE question about ONE server"),
        (vec!["data", "realtime", "frobnicate"], "unknown `data realtime` verb"),
    ] {
        let out = run(scratch.path(), &args);
        assert_eq!(out.status.code(), Some(2), "{args:?} is a usage error: {out:?}");
        let err = stderr(&out);
        assert!(err.contains(needle), "{args:?} must say {needle:?}: {err}");
    }
}

/// The group's help is the only place its verbs are named — it takes no default action — and a
/// `--help` that exited non-zero would break every `set -e` caller.
///
/// ⚠ The verb check reads the LABEL COLUMN rather than the page, because every verb name also
/// occurs in the surrounding prose: a `contains` would pass with a verb's whole block deleted, which
/// is the measured mistake `crates/vike-cli/src/cmd/data/catalog.rs`'s usage test records.
#[test]
fn realtime_help_names_both_verbs_and_exits_zero() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let out = run(scratch.path(), &["data", "realtime", "--help"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    // ⚠ `record` joined this list on 2026-09-22 and is a SUB-GROUP rather than a verb — it is
    // checked here all the same, because this page is the only place it is reachable from.
    for verb in ["watch", "status", "record"] {
        assert!(
            text.lines().any(|l| l.starts_with(&format!("  {verb}"))),
            "`{verb}` has no block of its own on the page: {text}"
        );
    }
    assert!(text.contains("STDOUT CARRIES FRAMES"), "the rule a pipeline depends on: {text}");
    // ...and the group is reachable from the PLANE's own page, or nobody finds it.
    let plane = stdout(&run(scratch.path(), &["data", "--help"]));
    assert!(plane.contains("realtime"), "{plane}");
    assert!(
        !plane.contains("designed and not built"),
        "the plane page may not still advertise this group's verbs as unbuilt: {plane}"
    );
}
