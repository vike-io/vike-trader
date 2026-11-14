//! The route's predicate, the backtest's mount facts, and the history channel end to end in one process
//! (a `PipeHistStore` on one end of two pipes, the parent's `answer` on the other).

use vike_backtesthub_client::proto::Request;
use vike_backtesthub_client::test_support::btcusdt_1m_bars;
use vike_backtesthub_client::wire_studio::StrategySpecWire;

use super::{facts_of, rust_file_of};
use crate::studio_run::StrategyFiles;

fn slice_request(spec: StrategySpecWire) -> Request {
    Request::RunSlice { spec, slice: Box::new(btcusdt_1m_bars()), params: None }
}

fn file(path: &str, params_toml: &str) -> Request {
    slice_request(StrategySpecWire::File { file: path.into(), params_toml: params_toml.into() })
}

/// A `.rs` FILE on a hub goes to a child, read as the run reads it (its text and sha256); a `.rhai` file,
/// an editor buffer, an artifact by (name, sha), a process that builds `.rs` files itself, and a file the
/// read refuses all stay in this process, where the run answers them in their own words.
#[test]
fn only_a_readable_rs_file_on_a_hub_goes_to_a_child() {
    let dir = tempfile::tempdir().expect("strategies folder");
    let text = "//! A.\n//!\n//! ```toml\n//! [strategy]\n//! runs_on = \"backtest\"\n//! [params.every]\n//! type = \"integer\"\n//! ```\npub fn x() {}\n";
    std::fs::create_dir_all(dir.path().join("other")).expect("folder");
    std::fs::write(dir.path().join("other/a.rs"), text).expect("a.rs");
    std::fs::write(dir.path().join("other/b.rhai"), text).expect("b.rhai");
    let hub = StrategyFiles::hub(dir.path().to_path_buf());

    let (path, source) = rust_file_of(&file("other/a.rs", "every = 3"), &hub).expect("routed");
    assert_eq!(path, "other/a.rs");
    assert_eq!(source.text, text);
    assert_eq!(source.sha256, vike_hub_protocol::hash::sha256_hex(text.as_bytes()));

    assert!(rust_file_of(&file("other/b.rhai", ""), &hub).is_none(), "Rhai runs here");
    assert!(rust_file_of(&file("other/missing.rs", ""), &hub).is_none(), "the read refuses it");
    assert!(rust_file_of(&file("other/a.rs", "fast = 1"), &hub).is_none(), "undeclared key");
    assert!(
        rust_file_of(
            &file("other/a.rs", ""),
            &StrategyFiles::stand_in(vike_strategies_check::stand_in)
        )
        .is_none()
    );
    assert!(
        rust_file_of(&slice_request(StrategySpecWire::Rhai("fn on_bar() {}".into())), &hub)
            .is_none()
    );
    let plugin = StrategySpecWire::Plugin {
        name: "a".into(),
        sha: "a".repeat(64),
        params_toml: String::new(),
    };
    assert!(rust_file_of(&slice_request(plugin), &hub).is_none(), "refused in this process");
    assert!(rust_file_of(&Request::Ping, &hub).is_none());
}

/// A backtest states its symbol and interval alone, and a request with no slice states nothing.
#[test]
fn a_backtest_states_its_symbol_and_interval_alone() {
    let facts: vike_model::params::MountFacts =
        serde_json::from_str(&facts_of(&slice_request(StrategySpecWire::Rhai(String::new()))))
            .expect("facts JSON");
    assert_eq!(
        facts,
        vike_model::params::MountFacts {
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            ..Default::default()
        }
    );
    let none = vike_model::params::MountFacts::default();
    assert_eq!(facts_of(&Request::Ping), serde_json::to_string(&none).expect("json"));
}

mod channel {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use vike_data::{HistoryStore, HistoryStoreError, HistoryStoreMemory, TimestampRange};
    use vike_marketdata::Bar;

    use super::super::pipe_store::PipeHistStore;
    use super::super::relay;
    use super::super::wire::{
        BARS_PER_FRAME, BarColumns, ChildFrame, ReadAnswerFrame, read_message, write_message,
    };

    /// `n` bars, the sixth with a NaN close and the seventh an infinite volume: msgpack keeps every bit,
    /// where JSON would have written `null` and refused to read it back.
    fn bars(n: usize) -> Vec<Bar> {
        (0..n)
            .map(|i| Bar {
                timestamp: 60_000 * i as i64,
                open: 100.0 + i as f64 / 7.0,
                high: 101.0 + i as f64 / 7.0,
                low: 99.0 + i as f64 / 7.0,
                close: if i == 5 { f64::NAN } else { 100.5 + i as f64 / 3.0 },
                volume: if i == 6 { f64::INFINITY } else { 1.0 / (i as f64 + 3.0) },
                funding: (i % 3 == 0).then_some(1e-4 * i as f64),
                bid: (i % 5 == 0).then_some(99.0),
                ask: None,
                // Runs of a symbol and of none, so the symbol column's run-length coding is crossed.
                symbol: (i % 7 < 4).then(|| "BTCUSDT".to_string()),
            })
            .collect()
    }

    /// A bar's every field, floats by their bits.
    fn bits(b: &Bar) -> (i64, [u64; 5], [Option<u64>; 3], Option<String>) {
        let f = [b.open, b.high, b.low, b.close, b.volume];
        let o = [b.funding, b.bid, b.ask];
        (b.timestamp, f.map(f64::to_bits), o.map(|v| v.map(f64::to_bits)), b.symbol.clone())
    }

    /// The child's store on one end, the parent's answers on the other: what `relay` does for every
    /// `ChildFrame::Read`, counted.
    fn serve(store: HistoryStoreMemory) -> (Arc<PipeHistStore>, Arc<AtomicUsize>) {
        let (mut from_child, child_out) = std::io::pipe().expect("pipe up");
        let (child_in, mut to_child) = std::io::pipe().expect("pipe down");
        let reads = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&reads);
        std::thread::spawn(move || {
            while let Ok(ChildFrame::Read(read)) = read_message(&mut from_child) {
                counted.fetch_add(1, Ordering::Relaxed);
                relay::answer(&mut to_child, &store, read).expect("answer");
            }
        });
        (Arc::new(PipeHistStore::new(Box::new(child_in), Box::new(child_out), Ok(None))), reads)
    }

    /// More rows than one part carries cross bit for bit, the second read of the same series comes from
    /// the cache, and the parent store's error and an unserved verb keep their words.
    #[test]
    fn the_parent_stores_answers_cross_whole_and_a_repeat_read_stays_in_the_child() {
        let rows = bars(BARS_PER_FRAME * 2 + 17);
        let store = HistoryStoreMemory::new();
        store.append_bars("binance", "BTCUSDT", "1m", &rows, None).expect("seed");
        let expected =
            store.load_bars("binance", "BTCUSDT", "1m", TimestampRange::all()).expect("read");
        let (pipe, reads) = serve(store);

        let got =
            pipe.load_bars("binance", "BTCUSDT", "1m", TimestampRange::all()).expect("piped read");
        assert_eq!(got.len(), rows.len());
        assert!(expected.iter().any(|b| b.close.is_nan()), "the NaN row is in the store");
        assert!(
            got.iter().map(bits).eq(expected.iter().map(bits)),
            "every bar must cross bit for bit, NaN and infinity included"
        );
        let again =
            pipe.load_bars("binance", "BTCUSDT", "1m", TimestampRange::all()).expect("cached");
        assert!(again.iter().map(bits).eq(got.iter().map(bits)));
        assert_eq!(reads.load(Ordering::Relaxed), 1, "the repeat read must not cross the channel");

        let empty =
            pipe.load_bars("binance", "ETHUSDT", "1m", TimestampRange::all()).expect("empty");
        assert!(empty.is_empty());

        match pipe.append_bars("binance", "BTCUSDT", "1m", &rows, None) {
            Err(HistoryStoreError::Unsupported(m)) => {
                assert!(m.contains("strategy sandbox"), "{m}")
            }
            other => panic!("a write must be refused inside the child: {other:?}"),
        }
    }

    /// A part the child cannot decode fails its read, and the rest of that answer is still read through
    /// `Done`: the next read gets its own answer (the failed one was not cached), not the last one's tail.
    #[test]
    fn a_part_the_child_cannot_decode_fails_its_read_and_keeps_the_channel_in_step() {
        let store = HistoryStoreMemory::new();
        store.append_bars("binance", "BTCUSDT", "1m", &bars(10), None).expect("seed");
        let (mut from_child, child_out) = std::io::pipe().expect("pipe up");
        let (child_in, mut to_child) = std::io::pipe().expect("pipe down");
        std::thread::spawn(move || {
            let mut first = true;
            while let Ok(ChildFrame::Read(read)) = read_message(&mut from_child) {
                if std::mem::take(&mut first) {
                    for garbage in ["not a bar part", "nor this"] {
                        write_message(&mut to_child, &ReadAnswerFrame::Rows(garbage))
                            .expect("part");
                    }
                    write_message(&mut to_child, &ReadAnswerFrame::<()>::Done).expect("done");
                } else {
                    relay::answer(&mut to_child, &store, read).expect("answer");
                }
            }
        });
        let pipe = PipeHistStore::new(Box::new(child_in), Box::new(child_out), Ok(None));
        match pipe.load_bars("binance", "BTCUSDT", "1m", TimestampRange::all()) {
            Err(HistoryStoreError::Io(m)) => assert!(m.contains("cannot decode"), "{m}"),
            other => panic!("a garbled part must fail its read: {other:?}"),
        }
        let again =
            pipe.load_bars("binance", "BTCUSDT", "1m", TimestampRange::all()).expect("next read");
        assert_eq!(again.len(), 10);
    }

    /// Every answer frame body written into `buffer`, in order.
    fn bodies(buffer: &[u8]) -> Vec<Vec<u8>> {
        let mut cursor = std::io::Cursor::new(buffer);
        std::iter::from_fn(|| vike_hub_protocol::frame::read_frame_raw(&mut cursor).ok()).collect()
    }

    /// **Where a sandboxed run's data cost goes**, MEASURED on 1,051,200 one-symbol bars (the
    /// `wf_momentum` window): the parent's store read (paid in-process too), the per-row NAMED msgpack the
    /// relay first used (encode, decode, and the clone the first cache made on insert), and the COLUMN frame
    /// that replaced it (encode, decode). Run in release, alone.
    #[test]
    #[ignore = "a measurement: cargo test --release -p vike-backtesthub --lib measure_the_bar_relay -- --ignored --nocapture"]
    fn measure_the_bar_relay() {
        let time = |f: &mut dyn FnMut()| {
            let t = std::time::Instant::now();
            f();
            t.elapsed()
        };
        let mut rows = bars(1_051_200);
        for b in &mut rows {
            b.symbol = Some("BTCUSDT".to_string());
        }
        let store = HistoryStoreMemory::new();
        store.append_bars("binance", "BTCUSDT", "1m", &rows, None).expect("seed");
        let mut read = Vec::new();
        let store_read = time(&mut || {
            read =
                store.load_bars("binance", "BTCUSDT", "1m", TimestampRange::all()).expect("read");
        });

        // The relay owns the store's answer, so the copies handed to the two encoders are made untimed.
        let (mut for_named, mut for_columns) = (Some(read.clone()), Some(read.clone()));
        let mut named = Vec::new();
        let named_encode = time(&mut || {
            let rows = for_named.take().expect("once");
            relay::send_rows(&mut named, Ok(rows)).expect("encode");
        });
        let named_frames = bodies(&named);
        let mut named_rows: Vec<Bar> = Vec::new();
        let named_decode = time(&mut || {
            for body in &named_frames {
                if let Ok(ReadAnswerFrame::Rows(mut part)) =
                    rmp_serde::from_slice::<ReadAnswerFrame<Vec<Bar>>>(body)
                {
                    named_rows.append(&mut part);
                }
            }
        });
        let mut kept = Vec::new();
        let clone = time(&mut || kept = named_rows.clone());

        let mut columns = Vec::new();
        let column_encode = time(&mut || {
            let rows = for_columns.take().expect("once");
            relay::send_bars(&mut columns, Ok(rows)).expect("encode");
        });
        let column_frames = bodies(&columns);
        let mut column_rows: Vec<Bar> = Vec::new();
        let column_decode = time(&mut || {
            for body in &column_frames {
                if let Ok(ReadAnswerFrame::Rows(part)) =
                    rmp_serde::from_slice::<ReadAnswerFrame<BarColumns<'_>>>(body)
                {
                    part.push_into(&mut column_rows).expect("columns");
                }
            }
        });
        assert!(named_rows.iter().map(bits).eq(read.iter().map(bits)), "named crosses whole");
        assert!(column_rows.iter().map(bits).eq(read.iter().map(bits)), "columns cross whole");
        assert_eq!(kept.len(), read.len());
        // The whole relay as the child meets it: the parent's store read and parallel encode on one
        // thread, the pipe, the child's parallel decode on this one.
        let (pipe, _) = serve(store);
        let mut piped = Vec::new();
        let relayed = time(&mut || {
            piped =
                pipe.load_bars("binance", "BTCUSDT", "1m", TimestampRange::all()).expect("piped");
        });
        assert!(piped.iter().map(bits).eq(read.iter().map(bits)), "the relay crosses whole");
        // A repeat read is the child's decode alone, from the bodies it cached.
        let mut again = Vec::new();
        let repeat = time(&mut || {
            again =
                pipe.load_bars("binance", "BTCUSDT", "1m", TimestampRange::all()).expect("cached");
        });
        assert_eq!(again.len(), read.len());
        println!(
            "relay of {} bars: store read {store_read:?} (in-process too)\n  named rows: {} bytes in {} \
             frames, encode {named_encode:?}, decode {named_decode:?}, cache clone {clone:?}\n  \
             columns:    {} bytes in {} frames, encode {column_encode:?}, decode {column_decode:?}\n  \
             the relay, store read included: {relayed:?}; a repeat read (the child's decode): {repeat:?}",
            read.len(),
            named.len(),
            named_frames.len(),
            columns.len(),
            column_frames.len()
        );
    }
}
