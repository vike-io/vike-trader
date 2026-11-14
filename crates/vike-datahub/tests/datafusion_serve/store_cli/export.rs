//! `store export` reads over the wire (0084's amendment) and refuses `--store` by name.

use std::net::TcpListener;
use std::sync::Arc;

use vike_data::HistoryStore;

use super::*;

/// A listener that serves NOTHING and only witnesses: a child pointed at it that connects leaves a
/// pending connection in its backlog, which [`assert_never_dialled`] then finds. The kernel
/// completes the handshake whether or not anything calls `accept`, so a dial cannot hide.
fn silent_hub() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    listener.set_nonblocking(true).expect("a non-blocking witness");
    let addr = listener.local_addr().expect("the bound address").to_string();
    (listener, addr)
}

fn assert_never_dialled(witness: &TcpListener, what: &str) {
    match witness.accept() {
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
        Err(e) => panic!("reading the witness: {e}"),
        Ok((_, peer)) => panic!("{what} dialled a datahub, from {peer}"),
    }
}

/// Serve `store` on a fresh loopback port, key-less: `vike-backend datahub --store DIR` in miniature.
fn serve(store: &Path) -> String {
    let hist = HistoryStoreDataFusion::open(store)
        .unwrap_or_else(|e| panic!("open the scratch store {}: {e}", store.display()));
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(hist);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port for the datahub");
    let addr = listener.local_addr().expect("the bound address").to_string();
    std::thread::spawn(move || {
        let _ = vike_datahub::serve(listener, store);
    });
    addr
}

/// **`store export` READS THROUGH A DATAHUB and writes exactly what the store holds.** It opened the
/// store at `--store` until 2026-09-26 — the one reader decision 0084's 2026-09-25 amendment left
/// open when it closed the local READ door — and it asks a datahub for the bars now; only the
/// Parquet encoding happens here.
///
/// ⚠ The assertion is on the BARS, bit for bit, against the store read DIRECTLY — not on "a file
/// appeared". The move put a JSON wire between the store and the encoder, and a float that crossed
/// it at the wrong precision would still write a valid file of the right length.
#[test]
fn data_export_reads_its_bars_through_a_datahub_and_writes_what_the_store_holds() {
    let (dir, store, _) = seeded();
    let hub = serve(&store);
    let file = dir.path().join("slice.parquet");
    let file_arg = file.to_str().expect("a UTF-8 path");

    let out = run_via(&hub, &["store", "export", "demo:BTCUSDT:1h", "--out", file_arg]);
    let stdout = stdout_of(&out);
    assert!(out.status.success(), "the export must succeed over the hub: {}", stderr_of(&out));
    assert!(
        stdout.contains(&format!("the datahub at {hub}")),
        "the line names where the bars came from: {stdout}"
    );

    let held = HistoryStoreDataFusion::open(&store)
        .expect("open the seeded store")
        .load_bars("demo", "BTCUSDT", "1h", vike_data::TimestampRange::all())
        .expect("read it directly");
    assert!(!held.is_empty(), "the demo tape seeds this series, so the comparison is not vacuous");
    // The row count LEADS the line — `scripts/publish_starter_data.sh` greps `^exported N`.
    assert!(
        stdout.starts_with(&format!("exported {} bars", held.len())),
        "the line leads with the number of bars the store holds: {stdout}"
    );

    // Loaded into a store that never saw the originals, so nothing can come from a cache.
    let fresh = HistoryStoreDataFusion::open(dir.path().join("fresh")).expect("a fresh store");
    let loaded = fresh
        .append_bars_from_parquet(&file, "demo", "BTCUSDT", "1h", Some("k"))
        .expect("the exported file loads");
    assert_eq!(loaded, held.len(), "the file holds a different number of bars than the store");
    let back = fresh
        .load_bars("demo", "BTCUSDT", "1h", vike_data::TimestampRange::all())
        .expect("read the import back");
    for (a, b) in held.iter().zip(&back) {
        assert_eq!(a.timestamp, b.timestamp);
        assert_eq!(a.open.to_bits(), b.open.to_bits(), "open at {}", a.timestamp);
        assert_eq!(a.high.to_bits(), b.high.to_bits(), "high at {}", a.timestamp);
        assert_eq!(a.low.to_bits(), b.low.to_bits(), "low at {}", a.timestamp);
        assert_eq!(a.close.to_bits(), b.close.to_bits(), "close at {}", a.timestamp);
        assert_eq!(a.volume.to_bits(), b.volume.to_bits(), "volume at {}", a.timestamp);
    }
}

/// **`--store` on `store export` is REFUSED BY NAME** — in both spellings and with no value at all —
/// with the one command that replaces it, and the refusal opens nothing, writes nothing and dials
/// nothing. Somebody who passed a directory believes it will be read; reading a datahub instead
/// while they believe that is the one outcome worse than refusing. A WRITER keeps its `--store`, for
/// the same line: the ruling was about readers.
#[test]
fn data_export_refuses_the_store_flag_by_name_and_touches_nothing() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let never = dir.path().join("never");
    let never_str = never.to_str().expect("a UTF-8 path");
    let file = dir.path().join("slice.parquet");
    let file_arg = file.to_str().expect("a UTF-8 path");
    let inline = format!("--store={never_str}");
    let (witness, addr) = silent_hub();

    for store_flag in [vec!["--store", never_str], vec![inline.as_str()], vec!["--store"]] {
        let argv: Vec<&str> = ["store", "export", "demo:BTCUSDT:1h", "--out", file_arg]
            .into_iter()
            .chain(store_flag.iter().copied())
            .collect();
        refuses_via(
            &addr,
            &argv,
            &["vike-datahub store export", "vike-backend datahub --store DIR", "decision 0084"],
        );
        assert!(!never.exists(), "{argv:?} created the store it was refused for");
        assert!(!file.exists(), "{argv:?} wrote an export file although it was refused");
    }
    assert_never_dialled(&witness, "`store export --store`");

    // …and a WRITER on the same binary keeps its `--store`: the ruling was about readers.
    seed_test_tape(&never);
    let rm = run(&[
        "store",
        "rm",
        "--kind",
        "bar",
        "--venue",
        "demo",
        "--symbol",
        "BTCUSDT",
        "--interval",
        "1h",
        "--dry-run",
        "--store",
        never_str,
    ]);
    assert!(rm.status.success(), "rm keeps --store: {}", stderr_of(&rm));
}

/// With no datahub to ask, the export FAILS — naming the address it dialled and the command that
/// serves local files — and leaves no file behind. That is the ruling's stated cost ("without a
/// datahub nothing can be computed"), and the message is what turns it into one command.
#[test]
fn data_export_with_no_datahub_fails_naming_the_local_replacement() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let file = dir.path().join("slice.parquet");
    let file_arg = file.to_str().expect("a UTF-8 path");

    let out = run(&["store", "export", "demo:BTCUSDT:1h", "--out", file_arg]);
    let err = stderr_of(&out);
    assert_eq!(out.status.code(), Some(2), "a read that could not happen exits 2: {err}");
    assert!(out.stdout.is_empty(), "no `exported` line for an export that did not happen");
    assert!(err.contains(NO_DATAHUB), "it names the address it dialled: {err}");
    assert!(err.contains("vike-backend datahub --store DIR"), "and the local route: {err}");
    assert!(!file.exists(), "a failed read must leave no file behind");
}

/// `export` reads through a datahub, so with none listening it fails AFTER its argv was read —
/// at the read, with the spec named. Every spelling of its flags reaches that point.
#[test]
fn data_export_accepts_every_spelling_up_to_the_datahub_read() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let out_file = dir.path().join("x.parquet");
    let out_file = out_file.to_str().expect("a UTF-8 path");
    let inline_out = format!("--out={out_file}");
    for args in [
        vec!["store", "export", "demo:BTCUSDT:1h", "--out", out_file],
        vec!["store", "export", "demo:BTCUSDT:1h", inline_out.as_str()],
        vec![
            "store",
            "export",
            "demo:BTCUSDT:1h",
            "--out",
            out_file,
            "--from",
            "2025-01-01T00",
            "--to",
            "2025-02-01T00",
        ],
        vec!["store", "export", "--out", out_file, "demo:BTCUSDT:1h", "--from=2025-01-01T00"],
    ] {
        let out = run(&args);
        let stderr = stderr_of(&out);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr:?}");
        assert!(
            stderr.contains("reading demo:BTCUSDT:1h from"),
            "{args:?} must be accepted and fail only at the datahub read; stderr: {stderr:?}"
        );
    }
    // `--store` on the reader is refused BY NAME, both spellings.
    for args in [
        vec!["store", "export", "demo:BTCUSDT:1h", "--out", out_file, "--store", "/srv/hist"],
        vec!["store", "export", "demo:BTCUSDT:1h", "--out", out_file, "--store=/srv/hist"],
    ] {
        let out = run(&args);
        let stderr = stderr_of(&out);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr:?}");
        assert!(stderr.contains("no longer reads history locally"), "{args:?}: {stderr:?}");
    }
}
