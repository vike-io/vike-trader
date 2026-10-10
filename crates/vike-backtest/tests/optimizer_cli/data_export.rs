//! `data export` reads over the wire (0084's amendment) and refuses `--store` by name.

use super::*;

// ---------------------------------------------------------------- 0084's amendment: `data export` reads over the wire

/// **`data export` READS THROUGH A DATAHUB and writes exactly what the store holds.** It opened the
/// store at `--store` until 2026-09-26 — the one reader decision 0084's 2026-09-25 amendment left
/// open when it closed the local READ door — and it asks a datahub for the bars now; only the
/// Parquet encoding happens in the engine.
///
/// ⚠ The assertion is on the BARS, bit for bit, against the store read DIRECTLY — not on "a file
/// appeared". The move put a JSON wire between the store and the encoder, and a float that crossed
/// it at the wrong precision would still write a valid file of the right length.
#[test]
fn data_export_reads_its_bars_through_a_datahub_and_writes_what_the_store_holds() {
    use vike_data::HistStore as _;

    let dir = tempfile::tempdir().expect("a scratch directory");
    let store = dir.path().join("hist");
    let store_arg = store.to_str().expect("a UTF-8 path");
    // The seed WRITES the store directly — writers keep `--store`; the export READS it over a hub.
    assert!(run(&["data", "seed-demo", "--store", store_arg]).status.success());
    let hub = local_datahub::serve(&store);
    let file = dir.path().join("slice.parquet");
    let file_arg = file.to_str().expect("a UTF-8 path");

    let out = run_via(&hub, &["data", "export", "demo:BTCUSDT:1h", "--out", file_arg]);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "the export must succeed over the hub: {}", stderr_of(&out));
    assert!(
        stdout.contains(&format!("the datahub at {hub}")),
        "the line names where the bars came from: {stdout}"
    );

    let held = vike_data::DataFusionHist::open(&store)
        .expect("open the seeded store")
        .load_bars("demo", "BTCUSDT", "1h", vike_data::TsRange::all())
        .expect("read it directly");
    assert!(!held.is_empty(), "the demo tape seeds this series, so the comparison is not vacuous");
    // The row count LEADS the line — `scripts/publish_starter_data.sh` greps `^exported N`.
    assert!(
        stdout.starts_with(&format!("exported {} bars", held.len())),
        "the line leads with the number of bars the store holds: {stdout}"
    );

    // Loaded into a store that never saw the originals, so nothing can come from a cache.
    let fresh = vike_data::DataFusionHist::open(dir.path().join("fresh")).expect("a fresh store");
    let loaded = fresh
        .append_bars_from_parquet(&file, "demo", "BTCUSDT", "1h", Some("k"))
        .expect("the exported file loads");
    assert_eq!(loaded, held.len(), "the file holds a different number of bars than the store");
    let back = fresh
        .load_bars("demo", "BTCUSDT", "1h", vike_data::TsRange::all())
        .expect("read the import back");
    for (a, b) in held.iter().zip(&back) {
        assert_eq!(a.ts, b.ts);
        assert_eq!(a.open.to_bits(), b.open.to_bits(), "open at {}", a.ts);
        assert_eq!(a.high.to_bits(), b.high.to_bits(), "high at {}", a.ts);
        assert_eq!(a.low.to_bits(), b.low.to_bits(), "low at {}", a.ts);
        assert_eq!(a.close.to_bits(), b.close.to_bits(), "close at {}", a.ts);
        assert_eq!(a.volume.to_bits(), b.volume.to_bits(), "volume at {}", a.ts);
    }
}

/// **`--store` on `data export` is REFUSED BY NAME** — in both spellings and with no value at all —
/// with the one command that replaces it, and the refusal opens nothing, writes nothing and dials
/// nothing. Somebody who passed a directory believes it will be read; reading a datahub instead
/// while they believe that is the one outcome worse than refusing.
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
        let argv: Vec<&str> = ["data", "export", "demo:BTCUSDT:1h", "--out", file_arg]
            .into_iter()
            .chain(store_flag.iter().copied())
            .collect();
        refuses_via(
            &addr,
            &argv,
            &["backtest data export", "VIKE_DATAHUB_STORE=DIR vike-backend datahub"],
        );
        assert!(!never.exists(), "{argv:?} created the store it was refused for");
        assert!(!file.exists(), "{argv:?} wrote an export file although it was refused");
    }
    assert_never_dialled(&witness, "`data export --store`");

    // …and a WRITER on the same binary keeps its `--store`: the ruling was about readers.
    let seed = run(&["data", "seed-demo", "--store", never_str]);
    assert!(seed.status.success(), "seed-demo writes and keeps --store: {}", stderr_of(&seed));
}

/// With no datahub to ask, the export FAILS — naming the address it dialled and the command that
/// serves local files — and leaves no file behind. That is the ruling's stated cost ("without a
/// datahub nothing can be computed"), and the message is what turns it into one command.
#[test]
fn data_export_with_no_datahub_fails_naming_the_local_replacement() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let file = dir.path().join("slice.parquet");
    let file_arg = file.to_str().expect("a UTF-8 path");

    let out = run(&["data", "export", "demo:BTCUSDT:1h", "--out", file_arg]);
    let err = stderr_of(&out);
    assert_eq!(out.status.code(), Some(2), "a read that could not happen exits 2: {err}");
    assert!(out.stdout.is_empty(), "no `exported` line for an export that did not happen");
    assert!(err.contains(NO_DATAHUB), "it names the address it dialled: {err}");
    assert!(
        err.contains("VIKE_DATAHUB_STORE=DIR vike-backend datahub"),
        "and the local route: {err}"
    );
    assert!(!file.exists(), "a failed read must leave no file behind");
}
