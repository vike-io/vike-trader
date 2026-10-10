use super::*;
use crate::store::datafusion_hist::codec::BarCodec;
use vike_model::Bar;

fn bars(n: usize) -> Vec<Bar> {
    (0..n)
        .map(|i| Bar {
            ts: i as i64 * 1000,
            open: 1.0,
            high: 2.0,
            low: 0.5,
            close: 1.5,
            volume: 10.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: Some("BTCUSDT.binance".to_string()),
        })
        .collect()
}

/// The regression for the the CI box `offset overflow` panic: one `date=` partition must NOT be
/// re-encoded into a single Arrow batch, because one `StringArray`'s i32 offsets cap a single
/// array at ~2 GiB and arrow PANICS (not `Err`s) past it — which killed the maintenance thread.
#[test]
fn compaction_encode_chunks_instead_of_building_one_giant_batch() {
    let schema = BarCodec::schema();
    let rows = bars(COMPACT_BATCH_ROWS + 7);
    let out = encode_chunked::<BarCodec>(&rows, &schema).unwrap();
    assert_eq!(out.len(), 2, "must split past the row cap, not emit one batch");
    assert_eq!(out[0].num_rows(), COMPACT_BATCH_ROWS);
    assert_eq!(out[1].num_rows(), 7);
    assert_eq!(out.iter().map(|b| b.num_rows()).sum::<usize>(), rows.len(), "chunking is lossless");
}

/// Under the cap the output is exactly what the old single-batch code produced — so the
/// overwhelming majority of compactions are unchanged.
#[test]
fn compaction_encode_is_one_batch_under_the_cap() {
    let schema = BarCodec::schema();
    let out = encode_chunked::<BarCodec>(&bars(1000), &schema).unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].num_rows(), 1000);
}

/// An empty compaction still yields one (empty) batch, as before — a valid part, not zero parts.
#[test]
fn compaction_encode_of_nothing_is_one_empty_batch() {
    let schema = BarCodec::schema();
    let out = encode_chunked::<BarCodec>(&bars(0), &schema).unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].num_rows(), 0);
}
